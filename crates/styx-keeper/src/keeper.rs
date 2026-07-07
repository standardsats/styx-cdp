//! The keeper: a purse (the wallet machinery - one p2tr key, coin scans, PSET signing), a
//! verified quote book, and one `step` per fresh tick.
//!
//! A step executes AT MOST ONE protocol action. Liquidations and pokes co-spend the
//! singletons (pot / reserve / issuer), whose successors are unconfirmed until the next
//! block - a second action in the same step would build on outpoints the indexer cannot
//! see yet. One action per block is also the natural pace of a watchtower.
//!
//! Priority: bad-debt > full-liq > partial > poke > refresh. Conflicts on broadcast are the
//! singleton contention mode (another keeper won the race): resync, rebuild, retry a
//! bounded number of times, and treat a vault that vanished meanwhile as resolved
//! elsewhere. A `Rejected` is different - the prune tier accepted what the node refused,
//! which means a builder invariant broke: alert and stop.

use std::time::Duration;

use styx_core::domain::{OnChain, VaultState};
use styx_core::elements::{OutPoint, Txid};
use styx_core::oracle::OracleTick;
use styx_core::units::{BlockHeight, Obol, Sats};
use styx_pset::build;
use styx_pset::intent::{
    BadDebtIntent, FullLiqIntent, FundingCoin, LiquidateIntent, ObolCoin, PokeIntent, RefreshIntent,
};
use styx_pset::plan::TxPlan;
use styx_wallet::wallet::{retry_conflicts, LostRace, Wallet, WalletError, FEE};
use styx_watch::quotes::{QuoteBook, WireQuote};
use styx_watch::transport::QuoteTransport;

use crate::decide::{decide, Action};

#[derive(Debug, Clone, Copy)]
pub struct KeeperOpts {
    /// POKE when the issuer anchor lags the tick by more than this many blocks.
    pub poke_lag: u32,
    /// REFRESH a healthy vault whose ratchet lags the tick by more than this many blocks.
    pub refresh_lag: u32,
    /// How far below the tip `assemble` walks looking for a quote quorum.
    pub walkback: u32,
}

impl Default for KeeperOpts {
    fn default() -> Self {
        KeeperOpts { poke_lag: 4, refresh_lag: 16, walkback: 8 }
    }
}

/// What a step did (one action per step by design).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Performed {
    Poked {
        txid: Txid,
    },
    Refreshed {
        vault: OutPoint,
        txid: Txid,
    },
    Partial {
        vault: OutPoint,
        dd: Obol,
        txid: Txid,
    },
    FullLiq {
        vault: OutPoint,
        txid: Txid,
    },
    BadDebt {
        vault: OutPoint,
        txid: Txid,
    },
    /// A conflict retry found the work already done by someone else.
    ResolvedElsewhere {
        vault: OutPoint,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum KeeperError {
    #[error(transparent)]
    Wallet(#[from] WalletError),
    /// The node rejected what the prune tier accepted: a builder invariant broke. This is
    /// a bug, not an operational condition - surface it loudly.
    #[error("ALERT {what}: node rejected a builder transaction: {message}")]
    Rejected { what: &'static str, message: String },
    #[error("{what}: still conflicted after {attempts} rebuilds")]
    ConflictExhausted { what: &'static str, attempts: u32 },
}

/// The exit code for a `Rejected` invariant break. Distinct from ordinary failures so the
/// service manager can be told NOT to restart it (`RestartPreventExitStatus=` in the
/// systemd unit): a restarted keeper would resync, reach the same decision, rebuild the
/// same transaction, and hit the same rejection every few seconds - an alert loop instead
/// of the intended full stop.
pub const REJECTED_EXIT: i32 = 65;

/// How many blocks an acted-on vault stays off limits while its spend awaits a block.
/// Normally the spend confirms with the next block and the outpoint leaves the index; if
/// the transaction fell out of the mempool instead, the keeper retries after this many.
const ACTED_RETRY_BLOCKS: u32 = 6;

/// The network fee for a `vsize`-vbyte transaction: the node's relay-floor rate with a small
/// headroom, clamped below the flat `FEE` (never regress, never exceed what a fixture-sized
/// coin covers) and above a dust-safe floor. Falls back to `FEE` when the node can't be
/// asked. This turns the ~100x flat overpay on Liquid's 0.1 sat/vb relay into a network-rate
/// fee - the flat FEE was sized for a 10x margin over 1 sat/vb.
fn network_fee(w: &Wallet, vsize: usize) -> Sats {
    const HEADROOM: u64 = 2;
    const FLOOR: u64 = 300;
    let Some(rate_kvb) = w.relay_feerate_sat_per_kvb() else {
        return FEE;
    };
    let base = rate_kvb.saturating_mul(vsize as u64).div_ceil(1000);
    Sats::new(base.saturating_mul(HEADROOM).clamp(FLOOR, FEE.raw()))
}

/// Build (via `build_plan`), sign, and broadcast a purse-funded op at the network fee. A
/// provisional build at the flat `FEE` reveals the real Simplicity-witness vsize; it rebuilds
/// at the estimated fee only when that is lower (the fee is an explicit fixed-width amount, so
/// the vsize does not move). Only for ops whose fee the covenant does not constrain - poke and
/// refresh, whose fee lands in the purse change.
fn broadcast_estimated(
    w: &Wallet,
    build_plan: impl Fn(Sats) -> Result<TxPlan, WalletError>,
) -> Result<Txid, WalletError> {
    let provisional = w.sign(&build_plan(FEE)?)?;
    let fee = network_fee(w, provisional.vsize());
    if fee.raw() >= FEE.raw() {
        return w.broadcast(&provisional);
    }
    w.broadcast(&w.sign(&build_plan(fee)?)?)
}

pub struct Keeper {
    /// The purse, shared: the daemon owns the only handle, styx-app hands the keeper the
    /// same session its op endpoints use (one set of coins, one writer at a time). Methods
    /// lock per call and never hold the guard across another lock.
    pub purse: std::sync::Arc<std::sync::Mutex<Wallet>>,
    pub book: QuoteBook,
    pub opts: KeeperOpts,
    /// Broadcast-but-unconfirmed actions: spent vault outpoint -> the height acted at.
    /// Polling faster than blocks confirm must not re-act on the same vault: the rebuild
    /// either duplicates the transaction or self-conflicts in the mempool.
    acted: std::collections::BTreeMap<OutPoint, u32>,
    /// The poke's mirror of `acted`: (the anchor when we poked, the height acted at).
    /// Until the anchor moves - our poke confirmed, or someone else's did - the lag is
    /// still measured against the OLD anchor and every step would re-poke into its own
    /// unconfirmed transaction.
    poked: Option<(BlockHeight, u32)>,
    /// The height of the last executed action. One action per BLOCK is the operating
    /// invariant (the module doc's "one action per step" is necessary but not sufficient
    /// when polling outpaces blocks): a second action in the same block would fight the
    /// first over the purse's coins - the confirmed-coin scan cannot see our own pending
    /// spends - and burn its conflict retries against our own mempool.
    last_acted_height: Option<u32>,
}

impl Keeper {
    pub fn new(purse: std::sync::Arc<std::sync::Mutex<Wallet>>, opts: KeeperOpts) -> Self {
        let oracle_pks = {
            let w = purse.lock().unwrap_or_else(|e| e.into_inner());
            w.ctx.params.oracle_pks
        };
        Keeper {
            purse,
            book: QuoteBook::new(oracle_pks),
            opts,
            acted: std::collections::BTreeMap::new(),
            poked: None,
            last_acted_height: None,
        }
    }

    /// The locked purse. Poison recovery matches the wallet's own convention: every step
    /// begins with a sync that rebuilds the view from the node.
    pub fn wallet(&self) -> std::sync::MutexGuard<'_, Wallet> {
        self.purse.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Feed one incoming quote into the book (verification inside; rejects are dropped).
    pub fn absorb(&mut self, quote: &WireQuote) {
        let tip = BlockHeight::new(self.wallet().state.height);
        if let Err(e) = self.book.insert(quote, tip) {
            eprintln!("dropping a quote: {e}");
        }
    }

    /// Drain whatever the transport has pending right now (bounded by the idle timeout).
    pub async fn drain<T: QuoteTransport>(&mut self, transport: &mut T) {
        while let Ok(Ok(q)) = tokio::time::timeout(Duration::from_millis(25), transport.recv()).await {
            self.absorb(&q);
        }
    }

    /// The freshest assemblable tick at or near the indexed tip.
    pub fn assemble(&self) -> Option<OracleTick> {
        let tip = self.wallet().state.height;
        (tip.saturating_sub(self.opts.walkback)..=tip)
            .rev()
            .find_map(|h| self.book.assemble_tick(BlockHeight::new(h)))
    }

    /// One watchtower pass: sync, decide over every known vault, execute the single most
    /// urgent action (see the module doc for why one). Vaults with a pending broadcast
    /// (`acted`) sit out until their spend confirms or goes stale.
    pub fn step(&mut self, tick: &OracleTick) -> Result<Option<Performed>, KeeperError> {
        // One guard for the pre-work, dropped before execute (whose retry loop locks per
        // attempt): the indexed facts are copied out, not borrowed.
        let (height, anchor, indexed) = {
            let mut w = self.wallet();
            w.sync()?;
            let anchor = w.protocol()?.issuer.state.last_mint_height;
            let indexed: std::collections::BTreeSet<OutPoint> = w.state.vaults.keys().copied().collect();
            (w.state.height, anchor, indexed)
        };
        // One action per block: if we already acted at this height, wait for it to land.
        if self.last_acted_height == Some(height) {
            return Ok(None);
        }
        // An acted-on outpoint that left the index confirmed; one that lingered past the
        // retry window is fair game again (the broadcast evidently went nowhere).
        self.acted
            .retain(|op, at| indexed.contains(op) && height < at.saturating_add(ACTED_RETRY_BLOCKS));

        // Liquidations, most severe first. Vaults with unresolved owners cannot be built
        // against (their owner bytes never surfaced); the indexer keeps them opaque.
        let rank = |a: &Action| match a {
            Action::BadDebt => 3,
            Action::FullLiq => 2,
            Action::Partial { .. } => 1,
            _ => 0,
        };
        let mut vaults = self.known_vaults();
        vaults.retain(|v| !self.acted.contains_key(&v.outpoint));
        vaults.sort_by_key(|v| v.outpoint); // deterministic order
        let urgent = vaults
            .iter()
            .map(|v| (*v, decide(v, tick, anchor, self.opts.refresh_lag, FEE)))
            .filter(|(_, a)| rank(a) > 0)
            .max_by_key(|(_, a)| rank(a));
        if let Some((vault, action)) = urgent {
            return self.execute_tracked(vault.outpoint, action, tick, height);
        }

        // Duties: the mint-anchor poke outranks refreshes (every issuer-gated op feeds on
        // its freshness), then the first stale healthy vault. A pending poke (anchor
        // unchanged since we broadcast, within the retry window) sits out like an acted
        // vault does.
        self.poked = self.poked.filter(|(at_anchor, at_height)| {
            *at_anchor == anchor && height < at_height.saturating_add(ACTED_RETRY_BLOCKS)
        });
        if self.poked.is_none() && tick.height().raw().saturating_sub(anchor.raw()) > self.opts.poke_lag
        {
            let txid = self.with_retry("poke", |k| k.build_poke(tick))?;
            if txid.is_some() {
                self.poked = Some((anchor, height));
                self.last_acted_height = Some(height);
            }
            return Ok(txid.map(|txid| Performed::Poked { txid }));
        }
        let stale = vaults
            .iter()
            .find(|v| decide(v, tick, anchor, self.opts.refresh_lag, FEE) == Action::Refresh)
            .copied();
        if let Some(vault) = stale {
            return self.execute_tracked(vault.outpoint, Action::Refresh, tick, height);
        }
        Ok(None)
    }

    /// Execute and, on a successful broadcast, put the vault on the acted list until the
    /// spend confirms.
    fn execute_tracked(
        &mut self,
        vault: OutPoint,
        action: Action,
        tick: &OracleTick,
        height: u32,
    ) -> Result<Option<Performed>, KeeperError> {
        let done = self.execute(vault, action, tick)?;
        if done.is_some() {
            self.acted.insert(vault, height);
            self.last_acted_height = Some(height);
        }
        Ok(done)
    }

    fn known_vaults(&self) -> Vec<OnChain<VaultState>> {
        self.wallet().state.vaults.iter().filter_map(|(op, v)| v.known(*op)).collect()
    }

    /// Execute one decided action against a vault, with the conflict-retry loop. The vault
    /// is re-read from the indexer on every attempt, so a rebuild is against fresh state; a
    /// vault gone after a conflict means another keeper resolved it.
    fn execute(
        &mut self,
        vault: OutPoint,
        action: Action,
        tick: &OracleTick,
    ) -> Result<Option<Performed>, KeeperError> {
        let txid = match action {
            Action::Partial { .. } => self.with_retry("partial liquidate", |k| {
                let v = k.vault_at(vault)?;
                let w = &*k.wallet();
                let protocol = w.protocol()?;
                // Re-plan against the re-read vault: a conflict may have changed it.
                let anchor = protocol.issuer.state.last_mint_height;
                let Action::Partial { dd, residual } = decide(&v, tick, anchor, k.opts.refresh_lag, FEE)
                else {
                    return Err(WalletError::VaultNotFound(vault)); // no longer partial: resolved
                };
                let (op, val) = w.ensure_obol(dd.raw())?;
                let built = build::liquidate::liquidate(
                    &w.ctx,
                    &protocol,
                    &v,
                    &LiquidateIntent {
                        dd,
                        residual,
                        keeper: ObolCoin { outpoint: op, value: Obol::new(val), spk: w.funding_spk() },
                        keeper_spk: w.funding_spk(),
                        obol_change_spk: w.funding_spk(),
                        tick: tick.clone(),
                        fee: FEE,
                    },
                )?;
                w.sign_and_broadcast(&built.plan)
            })?,
            Action::FullLiq => self.with_retry("full liquidate", |k| {
                let v = k.vault_at(vault)?;
                let w = &*k.wallet();
                let protocol = w.protocol()?;
                // Strictly more than the debt: the positive OBOL change is the E-5 anchor.
                let (op, val) = w.ensure_obol(v.state.debt.raw() + 1)?;
                let built = build::full_liq::full_liq(
                    &w.ctx,
                    &protocol,
                    &v,
                    &FullLiqIntent {
                        keeper: ObolCoin { outpoint: op, value: Obol::new(val), spk: w.funding_spk() },
                        keeper_spk: w.funding_spk(),
                        obol_change_spk: w.funding_spk(),
                        tick: tick.clone(),
                        fee: FEE,
                    },
                )?;
                w.sign_and_broadcast(&built.plan)
            })?,
            Action::BadDebt => self.with_retry("bad debt", |k| {
                let v = k.vault_at(vault)?;
                let w = &*k.wallet();
                let protocol = w.protocol()?;
                let (op, val) = w.ensure_obol(v.state.debt.raw())?;
                let (fee_op, fee_val) = w.pick_lbtc(FEE.raw() + 1)?;
                let built = build::bad_debt::bad_debt(
                    &w.ctx,
                    &protocol,
                    &v,
                    &BadDebtIntent {
                        keeper: ObolCoin { outpoint: op, value: Obol::new(val), spk: w.funding_spk() },
                        keeper_spk: w.funding_spk(),
                        obol_change_spk: w.funding_spk(),
                        fee_coin: FundingCoin {
                            outpoint: fee_op,
                            value: Sats::new(fee_val),
                            spk: w.funding_spk(),
                        },
                        change_spk: w.funding_spk(),
                        tick: tick.clone(),
                        fee: FEE,
                    },
                )?;
                w.sign_and_broadcast(&built.plan)
            })?,
            Action::Refresh => self.with_retry("refresh", |k| {
                let v = k.vault_at(vault)?;
                let w = &*k.wallet();
                let (fee_op, fee_val) = w.pick_lbtc(FEE.raw() + 1)?;
                broadcast_estimated(w, |fee| {
                    Ok(build::refresh::refresh(
                        &w.ctx,
                        &v,
                        &RefreshIntent {
                            tick: tick.clone(),
                            fee_coin: FundingCoin {
                                outpoint: fee_op,
                                value: Sats::new(fee_val),
                                spk: w.funding_spk(),
                            },
                            change_spk: w.funding_spk(),
                            fee,
                        },
                    )?
                    .plan)
                })
            })?,
            Action::None => return Ok(None),
        };
        Ok(Some(match (txid, action) {
            (None, _) => Performed::ResolvedElsewhere { vault },
            (Some(txid), Action::Partial { dd, .. }) => Performed::Partial { vault, dd, txid },
            (Some(txid), Action::FullLiq) => Performed::FullLiq { vault, txid },
            (Some(txid), Action::BadDebt) => Performed::BadDebt { vault, txid },
            (Some(txid), Action::Refresh) => Performed::Refreshed { vault, txid },
            (Some(_), Action::None) => unreachable!("handled above"),
        }))
    }

    fn vault_at(&self, op: OutPoint) -> Result<OnChain<VaultState>, WalletError> {
        self.wallet()
            .state
            .vaults
            .get(&op)
            .and_then(|v| v.known(op))
            .ok_or(WalletError::VaultNotFound(op))
    }

    fn build_poke(&mut self, tick: &OracleTick) -> Result<Txid, WalletError> {
        let w = &*self.wallet();
        let protocol = w.protocol()?;
        let (op, val) = w.pick_lbtc(FEE.raw() + 1)?;
        broadcast_estimated(w, |fee| {
            Ok(build::poke::poke(
                &w.ctx,
                &protocol.issuer,
                &PokeIntent {
                    tick: tick.clone(),
                    funding: FundingCoin { outpoint: op, value: Sats::new(val), spk: w.funding_spk() },
                    change_spk: w.funding_spk(),
                    fee,
                },
            )?
            .plan)
        })
    }

    /// The shared conflict-retry (styx-wallet's), with the keeper's lost-race policy:
    /// losing a liquidation race to another keeper is the system working. The wallet-level
    /// alert variants map onto the keeper's (a Rejected keeps its exit-65 semantics).
    fn with_retry(
        &mut self,
        what: &'static str,
        f: impl FnMut(&mut Self) -> Result<Txid, WalletError>,
    ) -> Result<Option<Txid>, KeeperError> {
        retry_conflicts(what, self, |k| k.wallet().sync().map(|_| ()), f, LostRace::Benign).map_err(
            |e| match e {
                WalletError::Rejected { what, message } => KeeperError::Rejected { what, message },
                WalletError::ConflictExhausted { what, attempts } => {
                    KeeperError::ConflictExhausted { what, attempts }
                }
                e => KeeperError::Wallet(e),
            },
        )
    }
}
