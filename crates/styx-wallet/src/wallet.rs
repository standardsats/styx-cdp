//! The wallet session: the compiled deployment, the node connection, the indexer state, the
//! two keys, and the coin/vault views every op builds on.
//!
//! All funds live at ONE key-path p2tr script (the funding key): L-BTC funding coins, OBOL
//! principal and change alike. Ops discover coins by scanning that spk per asset, sign any
//! input paying it via `sign_funding` (SIGHASH_ALL, E-5), and send every change output back
//! to it. Vault ownership is separate: the owner key never appears on chain as an address,
//! only inside vault address commitments - our vaults are whatever the indexer resolved to
//! the owner pubkey.

use std::path::PathBuf;

use styx_core::artifacts::Artifacts;
use styx_core::domain::{OnChain, ProtocolState, VaultState};
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::elements::{Address, AddressParams, BlockHash, OutPoint, Script, Transaction, Txid};
use styx_core::units::{MathError, Sats};
use styx_node::client::Node;
use styx_node::scan::scan_spk;
use styx_node::{BroadcastError, NodeError, ScanError};
use styx_pset::error::{BuildError, PruneRejected};
use styx_pset::plan::TxPlan;
use styx_pset::pset::{finalize_pset, sign_funding, to_pset, PsetError};
use styx_pset::Ctx;
use styx_watch::config::StyxnetConfig;
use styx_watch::index::{IndexState, Notice};
use styx_watch::snapshot::{self, SnapshotError};
use styx_watch::sync::{catch_up, SyncError};

use crate::config::{ConfigError, WalletConfig};

/// The standard tx fee, matching the prune-tier fixtures and the other harnesses.
pub const FEE: Sats = styx_node::client::FEE;

#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("styxnet.toml: {0}")]
    Styxnet(#[from] styx_watch::config::ConfigError),
    #[error(transparent)]
    Node(#[from] NodeError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    Pset(#[from] PsetError),
    #[error(transparent)]
    Prune(#[from] PruneRejected),
    #[error(transparent)]
    Broadcast(#[from] BroadcastError),
    #[error(transparent)]
    Math(#[from] MathError),
    #[error("artifacts: {0}")]
    Artifacts(String),
    #[error("genesis mismatch: styxnet.toml says {config}, the node says {node}")]
    GenesisMismatch { config: BlockHash, node: BlockHash },
    #[error("the protocol is not live on this chain yet (sync first, or deploy)")]
    NotLive,
    #[error("the node is still syncing: height {tip}, the deployment starts at {anchor} - wait for sync and retry")]
    NodeBehindAnchor { tip: u32, anchor: u32 },
    #[error("vault {0} is not tracked")]
    VaultNotFound(OutPoint),
    #[error("vault {0} is not ours (or its owner is unresolved)")]
    VaultNotMine(OutPoint),
    #[error("need {need} sats of funding, the wallet holds {have} (fund it first)")]
    InsufficientLbtc { need: u64, have: u64 },
    #[error("need {need} OBOL, the wallet holds {have} (repay less, or acquire OBOL)")]
    InsufficientObol { need: u64, have: u64 },
    #[error("this wallet has {0} vaults; pass --vault to pick one")]
    AmbiguousVault(usize),
    #[error("no vault")]
    NoVault,
    /// The node rejected what the prune tier accepted: a builder invariant broke. This is
    /// a bug, not an operational condition.
    #[error("ALERT {what}: node rejected a builder transaction: {message}")]
    Rejected { what: &'static str, message: String },
    #[error("{what}: still conflicted after {attempts} rebuilds (heavy contention; retry)")]
    ConflictExhausted { what: &'static str, attempts: u32 },
}

impl WalletError {
    /// True when the failure is the node being unreachable or mid-restart, not anything
    /// about the wallet's own state. The daemons tolerate these for a bounded window
    /// instead of dying: an elementsd restart (or its container still warming up) must
    /// not take down a keeper that will not be auto-restarted.
    pub fn is_node_down(&self) -> bool {
        matches!(
            self,
            WalletError::Node(_)
                | WalletError::Scan(ScanError::Node(_))
                | WalletError::Sync(SyncError::Node(_))
        )
    }
}

/// What a vanished target after a conflict means to the caller. A keeper losing a race is
/// the system working (someone else liquidated first); an owner's op dissolving under
/// them wants their eyes, not a silent success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostRace {
    Benign,
    Error,
}

pub const CONFLICT_ATTEMPTS: u32 = 3;

/// The conflict-retry policy, shared by the wallet ops and the keeper and free-standing so
/// it is testable without a node. A `Conflict` on broadcast is singleton contention
/// (someone spent the pot / issuer / vault first): resync, rebuild from fresh state, retry
/// a bounded number of times. `Ok(None)` (under `LostRace::Benign` only) means the work
/// turned out to be done by someone else - the target vanished or its ratchet advanced
/// past our tick after a conflict-triggered resync.
pub fn retry_conflicts<S>(
    what: &'static str,
    state: &mut S,
    resync: impl Fn(&mut S) -> Result<(), WalletError>,
    mut f: impl FnMut(&mut S) -> Result<Txid, WalletError>,
    lost_race: LostRace,
) -> Result<Option<Txid>, WalletError> {
    for attempt in 0..CONFLICT_ATTEMPTS {
        match f(state) {
            Ok(txid) => return Ok(Some(txid)),
            Err(WalletError::Broadcast(BroadcastError::Conflict(m))) => {
                eprintln!("{what}: conflict ({m}), resync + rebuild ({attempt})");
                resync(state)?;
            }
            Err(WalletError::Broadcast(BroadcastError::Rejected(message))) => {
                return Err(WalletError::Rejected { what, message });
            }
            // The target vanished (or its ratchet advanced past our tick) while retrying.
            Err(
                e @ (WalletError::VaultNotFound(_)
                | WalletError::VaultNotMine(_)
                | WalletError::Build(BuildError::RatchetNotAdvanced { .. })),
            ) if attempt > 0 => {
                return match lost_race {
                    LostRace::Benign => Ok(None),
                    LostRace::Error => Err(e),
                };
            }
            Err(e) => return Err(e),
        }
    }
    Err(WalletError::ConflictExhausted { what, attempts: CONFLICT_ATTEMPTS })
}

pub struct Wallet {
    pub ctx: Ctx,
    pub node: Node,
    pub owner: zkp::Keypair,
    pub funding: zkp::Keypair,
    pub state: IndexState,
    snapshot_path: PathBuf,
    addr_params: &'static AddressParams,
}

pub use styx_core::net::address_params;

impl Wallet {
    /// Load everything a command needs: styxnet.toml -> params -> artifacts, the node (its
    /// genesis must match the config's - a wallet pointed at the wrong chain must refuse
    /// before it signs anything), and the persisted indexer state.
    pub fn open_session(cfg: &WalletConfig) -> Result<Wallet, WalletError> {
        let net = StyxnetConfig::load(&cfg.styxnet)?;
        let params = net.params()?;
        let node = Node::from_url(&cfg.rpc_url, cfg.auth()?)?;
        let genesis = node.genesis()?;
        let expected = net.genesis()?;
        if expected != genesis {
            return Err(WalletError::GenesisMismatch { config: expected, node: genesis });
        }
        let artifacts =
            Artifacts::compile(&params).map_err(|e| WalletError::Artifacts(e.to_string()))?;
        let ctx = Ctx { params, artifacts, genesis };
        let state = if cfg.snapshot.exists() {
            snapshot::load(&cfg.snapshot)?
        } else {
            // First sync starts at the deployment anchor, not block 1: nothing
            // protocol-relevant exists before the ceremony, and the real testnet is
            // millions of blocks deep.
            match &net.protocol {
                Some(p) => {
                    let start = p.issuer_anchor_genesis;
                    let tip = node.height()?;
                    if tip < start {
                        return Err(WalletError::NodeBehindAnchor { tip, anchor: start });
                    }
                    IndexState::at_height(start, node.block_hash(start)?)
                }
                None => IndexState::genesis(genesis),
            }
        };
        Ok(Wallet {
            ctx,
            node,
            owner: cfg.owner()?,
            funding: cfg.funding()?,
            state,
            snapshot_path: cfg.snapshot.clone(),
            addr_params: address_params(&net.network.chain),
        })
    }

    /// Catch the indexer up to the tip and persist the snapshot.
    pub fn sync(&mut self) -> Result<Vec<Notice>, WalletError> {
        let owners = [self.owner_pk()];
        let notices = catch_up(&self.node, &self.ctx, &owners, &mut self.state)?;
        snapshot::save(&self.state, &self.snapshot_path)?;
        Ok(notices)
    }

    pub fn owner_pk(&self) -> XOnlyPublicKey {
        self.owner.x_only_public_key().0
    }

    /// The wallet's one script: key-path p2tr over the funding key (no script tree), the
    /// shape `sign_funding` tweaks for.
    pub fn funding_spk(&self) -> Script {
        self.funding_address().script_pubkey()
    }

    pub fn funding_address(&self) -> Address {
        Address::p2tr(
            styx_core::secp(),
            self.funding.x_only_public_key().0,
            None,
            None,
            self.addr_params,
        )
    }

    /// Confirmed L-BTC coins at the funding spk.
    pub fn lbtc_coins(&self) -> Result<Vec<(OutPoint, u64)>, WalletError> {
        Ok(scan_spk(&self.node, &self.funding_spk(), self.ctx.params.policy)?)
    }

    /// Confirmed OBOL coins at the funding spk.
    pub fn obol_coins(&self) -> Result<Vec<(OutPoint, u64)>, WalletError> {
        Ok(scan_spk(&self.node, &self.funding_spk(), self.ctx.params.obol)?)
    }

    /// Our vaults: tracked, owner-resolved to our key.
    pub fn my_vaults(&self) -> Vec<OnChain<VaultState>> {
        let me = self.owner_pk();
        self.state
            .vaults
            .iter()
            .filter(|(_, v)| v.owner == Some(me))
            .filter_map(|(op, v)| v.known(*op))
            .collect()
    }

    /// The vault an op targets: the given outpoint, or the only vault we have.
    pub fn pick_vault(&self, which: Option<OutPoint>) -> Result<OnChain<VaultState>, WalletError> {
        match which {
            Some(op) => {
                let v = self.state.vaults.get(&op).ok_or(WalletError::VaultNotFound(op))?;
                if v.owner != Some(self.owner_pk()) {
                    return Err(WalletError::VaultNotMine(op));
                }
                v.known(op).ok_or(WalletError::VaultNotMine(op))
            }
            None => {
                let mine = self.my_vaults();
                match mine.len() {
                    0 => Err(WalletError::NoVault),
                    1 => Ok(mine[0]),
                    n => Err(WalletError::AmbiguousVault(n)),
                }
            }
        }
    }

    pub fn protocol(&self) -> Result<ProtocolState, WalletError> {
        self.state.protocol().ok_or(WalletError::NotLive)
    }

    /// Sign every input paying the funding spk (SIGHASH_ALL key-path) and broadcast. The
    /// caller mines (or waits for the producer); the wallet never assumes mining rights.
    /// Sign a plan's funding inputs and finalize it to a broadcastable transaction, WITHOUT
    /// sending. Split from `sign_and_broadcast` so a caller can measure the signed vsize (the
    /// Simplicity witness is only present here) before deciding the fee.
    pub fn sign(&self, plan: &TxPlan) -> Result<Transaction, WalletError> {
        let mut pset = to_pset(&self.ctx, plan)?;
        let ours = self.funding_spk();
        for (i, u) in plan.in_utxos.iter().enumerate() {
            if u.script_pubkey == ours {
                sign_funding(&self.ctx, plan, &mut pset, i, &self.funding)?;
            }
        }
        Ok(finalize_pset(&self.ctx, plan, &pset)?)
    }

    pub fn broadcast(&self, tx: &Transaction) -> Result<Txid, WalletError> {
        Ok(self.node.send(tx)?)
    }

    pub fn sign_and_broadcast(&self, plan: &TxPlan) -> Result<Txid, WalletError> {
        let tx = self.sign(plan)?;
        self.broadcast(&tx)
    }

    /// The node's relay-floor fee rate in sat/kvB, for network-rate fee selection.
    pub fn relay_feerate_sat_per_kvb(&self) -> Option<u64> {
        self.node.relay_feerate_sat_per_kvb()
    }

    /// The smallest single coin covering `need`.
    pub fn select_at_least(coins: &[(OutPoint, u64)], need: u64) -> Option<(OutPoint, u64)> {
        coins.iter().filter(|(_, v)| *v >= need).min_by_key(|(_, v)| *v).copied()
    }

    /// Largest-first accumulation up to `need`; None if the total falls short.
    pub fn select_accumulate(coins: &[(OutPoint, u64)], need: u64) -> Option<Vec<(OutPoint, u64)>> {
        let mut sorted: Vec<_> = coins.to_vec();
        sorted.sort_by_key(|(_, v)| std::cmp::Reverse(*v));
        let mut taken = Vec::new();
        let mut sum = 0u64;
        for c in sorted {
            taken.push(c);
            sum = sum.saturating_add(c.1);
            if sum >= need {
                return Some(taken);
            }
        }
        None
    }

    /// A plain self-spend that shapes an exact coin at the funding spk (OPEN's frozen layout
    /// has no change output, so its funding must be exact). Returns the exact coin's
    /// outpoint; the change stays at the funding spk. No covenant slots: the plan is all
    /// key-path inputs.
    pub fn shape_exact(&self, need: Sats) -> Result<(OutPoint, Transaction), WalletError> {
        use styx_pset::layout::{claimed, fee_out, txin, txout};
        let coins = self.lbtc_coins()?;
        let total_needed = need.raw() + FEE.raw();
        let have: u64 = coins.iter().map(|(_, v)| *v).sum();
        let taken = Self::select_accumulate(&coins, total_needed)
            .ok_or(WalletError::InsufficientLbtc { need: total_needed, have })?;
        let sum: u64 = taken.iter().map(|(_, v)| *v).sum();
        let ours = self.funding_spk();
        let mut output = vec![txout(need.raw(), ours.clone(), self.ctx.params.policy)];
        let (change, fee) = split_change(sum - need.raw() - FEE.raw());
        if let Some(c) = change {
            output.push(txout(c, ours.clone(), self.ctx.params.policy));
        }
        output.push(fee_out(fee, self.ctx.params.policy));
        let tx = Transaction {
            version: 2,
            lock_time: styx_core::elements::LockTime::ZERO,
            input: taken.iter().map(|(op, _)| txin(*op)).collect(),
            output,
        };
        let in_utxos =
            taken.iter().map(|(_, v)| claimed(*v, ours.clone(), self.ctx.params.policy)).collect();
        let plan = TxPlan { tx: tx.clone(), in_utxos, slots: vec![] };
        let txid = self.sign_and_broadcast(&plan)?;
        Ok((OutPoint::new(txid, 0), tx))
    }

    /// Merge every OBOL coin into one at the funding spk, the fee paid from an L-BTC coin.
    /// The single-payer-input builder layouts (repay / close / redeem) need one coin
    /// covering the amount, and OBOL change fragments over time - this is the wallet's
    /// consolidation move, run automatically when fragmentation blocks an op.
    pub fn consolidate_obol(&self) -> Result<(OutPoint, u64), WalletError> {
        use styx_pset::layout::{claimed, fee_out, txin, txout};
        let obol = self.obol_coins()?;
        let total: u64 = obol.iter().map(|(_, v)| *v).sum();
        let (fee_op, fee_val) = Self::select_at_least(&self.lbtc_coins()?, FEE.raw() + 1)
            .ok_or(WalletError::InsufficientLbtc { need: FEE.raw() + 1, have: 0 })?;
        let ours = self.funding_spk();
        let mut input: Vec<_> = obol.iter().map(|(op, _)| txin(*op)).collect();
        input.push(txin(fee_op));
        let mut in_utxos: Vec<_> =
            obol.iter().map(|(_, v)| claimed(*v, ours.clone(), self.ctx.params.obol)).collect();
        in_utxos.push(claimed(fee_val, ours.clone(), self.ctx.params.policy));
        let mut output = vec![txout(total, ours.clone(), self.ctx.params.obol)];
        let (change, fee) = split_change(fee_val - FEE.raw());
        if let Some(c) = change {
            output.push(txout(c, ours.clone(), self.ctx.params.policy));
        }
        output.push(fee_out(fee, self.ctx.params.policy));
        let tx =
            Transaction { version: 2, lock_time: styx_core::elements::LockTime::ZERO, input, output };
        let plan = TxPlan { tx, in_utxos, slots: vec![] };
        let txid = self.sign_and_broadcast(&plan)?;
        Ok((OutPoint::new(txid, 0), total))
    }
}

/// A p2tr output below the relay dust threshold would get the whole transaction rejected,
/// so change that small folds into the fee instead. ~330 sats is the core policy floor for
/// p2tr; 500 keeps margin.
pub const DUST_FLOOR: u64 = 500;

/// Split an excess into (change output, fee): dust-or-nothing folds into the fee.
pub fn split_change(excess: u64) -> (Option<u64>, Sats) {
    if excess >= DUST_FLOOR {
        (Some(excess), FEE)
    } else {
        (None, Sats::new(FEE.raw() + excess))
    }
}
