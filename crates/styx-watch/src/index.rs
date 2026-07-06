//! The chain indexer: protocol state inferred from transaction layout alone.
//!
//! Vault addresses derive from state and cannot be enumerated, but every mint serializes
//! through the issuer singleton, so the whole state machine is readable off the transaction
//! skeleton - no witness decoding. Dispatch keys on what the covenants themselves pin: the
//! issuer arms send the token to a fixed successor output (0 POKE / 4 OPEN / 3 DRAW and
//! ATTEST, told apart by the reserve co-spend), the vault arms keep the vault at input 0 and
//! its successor at output 0, the pot successor is located by its constant spk. The
//! builder-conventional secondary indices are verified on top, and any mismatch degrades to
//! an `Anomaly` or a lost vault - never a false record:
//!
//! - births: OPEN mints a vault at output 0; its debt is the pot delta;
//! - transitions: REPAY / REFRESH / DRAW move the vault at input 0 to output 0; partial
//!   LIQUIDATE and REDEEM are layout-isomorphic (both recurse to (debt - pot delta, owner,
//!   same last_height)), reported as one `Deleveraged` event;
//! - removals: CLOSE / FULL-LIQ / BAD-DEBT.
//!
//! Heights need care: the covenants commit the ORACLE TICK height into the successor spk,
//! and `check_lock_height` only makes nLockTime a lower bound for it (issuer.simf:19), so
//! nLockTime bounds the committed height from above but need not equal it. Anchors and
//! ratchets are therefore recovered by a bounded descending scan over candidate heights,
//! matched trustlessly against the successor's derived spk (the same move `issuer_birth`
//! makes). The one place a scan cannot decide is an opaque vault's REFRESH, where the
//! recorded last_height is an upper bound.
//!
//! The owner is the one field a layout cannot yield (it lives in the witness and the address
//! commitment only), so the indexer takes candidate owner keys - the wallet's filter seam -
//! and verifies `vault_spk(debt, candidate, last_height)` against the actual output; a match
//! is trustless. A vault with no matching candidate is tracked as *opaque*: same debt /
//! last_height / value bookkeeping, no successor-spk verification, unusable by builders until
//! its owner is learned. A recognized vault whose spend fits no covenant layout is marked
//! *lost* (the covenant would never produce it, so it means our picture diverged).
//!
//! Everything here is pure: `apply_tx` consumes transactions in chain order; IO lives in
//! `sync`.

use std::collections::BTreeMap;

use styx_core::artifacts::Ctx;
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, ReserveState, VaultState};
use styx_core::elements::confidential::{Asset as CAsset, Value as CValue};
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::elements::{AssetId, BlockHash, OutPoint, Script, Transaction, TxOut, Txid};
use styx_core::units::{BlockHeight, Obol, Sats};

/// A tracked vault. `owner: Some` means resolved and spk-verified; `None` means opaque - the
/// debt / last_height / value bookkeeping still holds (the covenant enforces the layout it
/// was inferred from), but successors cannot be verified and builders cannot spend it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedVault {
    pub debt: Obol,
    pub last_height: BlockHeight,
    pub value: Sats,
    pub owner: Option<XOnlyPublicKey>,
    /// The observed scriptPubKey: redundant for resolved vaults, the only address fact we
    /// have for opaque ones (late owner resolution re-checks candidates against it).
    pub spk: Script,
}

impl TrackedVault {
    /// The builder-consumable form, available once the owner is resolved.
    pub fn known(&self, outpoint: OutPoint) -> Option<OnChain<VaultState>> {
        let owner = self.owner?;
        Some(OnChain {
            state: VaultState { debt: self.debt, owner, last_height: self.last_height },
            outpoint,
            value: self.value,
        })
    }
}

/// Why a vault left tracking without a recognized transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lost {
    pub txid: Txid,
    pub reason: String,
}

/// What one transaction did to the protocol state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PotBorn {
        outpoint: OutPoint,
        value: Obol,
    },
    ReserveBorn {
        outpoint: OutPoint,
        value: Sats,
    },
    IssuerBorn {
        outpoint: OutPoint,
        anchor: BlockHeight,
    },
    /// A pure anchor advance (POKE). OPEN / DRAW / BAD-DEBT advance it too, inside their own
    /// events.
    Poked {
        anchor: BlockHeight,
    },
    Opened {
        vault: OutPoint,
        debt: Obol,
        value: Sats,
        last_height: BlockHeight,
        owner: Option<XOnlyPublicKey>,
    },
    Repaid {
        vault: OutPoint,
        successor: OutPoint,
        amount: Obol,
    },
    Drawn {
        vault: OutPoint,
        successor: OutPoint,
        amount: Obol,
    },
    Refreshed {
        vault: OutPoint,
        successor: OutPoint,
        last_height: BlockHeight,
    },
    /// Partial LIQUIDATE or REDEEM: the layouts are isomorphic (vault at input 0, reserve at
    /// input 3, pot grows by the amount, successor keeps last_height), so the indexer does
    /// not pretend to tell them apart.
    Deleveraged {
        vault: OutPoint,
        successor: OutPoint,
        amount: Obol,
    },
    Closed {
        vault: OutPoint,
    },
    FullyLiquidated {
        vault: OutPoint,
    },
    BadDebtClosed {
        vault: OutPoint,
    },
    VaultLost {
        vault: OutPoint,
        reason: &'static str,
    },
    /// A standalone donation into a constant-address singleton (permissionless arms).
    PotGrew {
        value: Obol,
    },
    ReserveGrew {
        value: Sats,
    },
    /// A second same-asset UTXO at a constant protocol address: the L-3 fragmentation threat
    /// scan_protocol refuses on. Does not move the snapshot.
    Fragment {
        role: &'static str,
    },
    /// A layout that contradicts what the covenants can produce. On a real chain this means
    /// the indexer's picture diverged; the state is left as intact as possible.
    Anomaly {
        what: &'static str,
    },
}

/// An event, stamped with where it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub height: u32,
    pub txid: Txid,
    pub event: Event,
}

/// The indexed protocol state: everything a daemon knows about the chain, serializable as
/// the snapshot file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexState {
    /// The last fully processed block height and its hash (the reorg check anchor).
    pub height: u32,
    pub tip: BlockHash,
    pub pot: Option<OnChain<PotState>>,
    pub reserve: Option<OnChain<ReserveState>>,
    pub issuer: Option<OnChain<IssuerState>>,
    pub vaults: BTreeMap<OutPoint, TrackedVault>,
    pub lost: BTreeMap<OutPoint, Lost>,
}

impl IndexState {
    /// An empty index at the chain's genesis: nothing deployed yet, scan starts at block 1.
    pub fn genesis(tip: BlockHash) -> Self {
        Self::at_height(0, tip)
    }

    /// An empty index sealed at an arbitrary height: the scan starts at `height + 1`. The
    /// caller asserts nothing protocol-relevant exists at or before `height` - the intended
    /// use is starting at the deployment anchor from styxnet.toml, where that holds by
    /// construction (the ceremony's transactions land strictly after the recorded anchor).
    /// On the real testnet this is the difference between seconds and hours of first sync.
    pub fn at_height(height: u32, tip: BlockHash) -> Self {
        IndexState {
            height,
            tip,
            pot: None,
            reserve: None,
            issuer: None,
            vaults: BTreeMap::new(),
            lost: BTreeMap::new(),
        }
    }

    /// The builder-consumable snapshot, once all three singletons are live.
    pub fn protocol(&self) -> Option<ProtocolState> {
        Some(ProtocolState { pot: self.pot?, reserve: self.reserve?, issuer: self.issuer? })
    }

    /// Mark a block fully processed.
    pub fn seal(&mut self, height: u32, hash: BlockHash) {
        self.height = height;
        self.tip = hash;
    }

    /// Process one transaction. Call in chain order (blocks ascending, txs in block order);
    /// `height` is the block being processed.
    pub fn apply_tx(
        &mut self,
        ctx: &Ctx,
        owners: &[XOnlyPublicKey],
        height: u32,
        tx: &Transaction,
    ) -> Vec<Notice> {
        let mut ev = Notices { height, txid: tx.txid(), out: Vec::new() };

        let issuer_in = self.issuer.and_then(|i| input_index(tx, i.outpoint));
        let pot_in = self.pot.and_then(|p| input_index(tx, p.outpoint));
        let reserve_in = self.reserve.and_then(|r| input_index(tx, r.outpoint));
        let vault_in: Vec<(usize, OutPoint)> = tx
            .input
            .iter()
            .enumerate()
            .filter(|(_, i)| self.vaults.contains_key(&i.previous_output))
            .map(|(n, i)| (n, i.previous_output))
            .collect();

        if issuer_in.is_none() && pot_in.is_none() && reserve_in.is_none() && vault_in.is_empty() {
            self.non_protocol_tx(ctx, tx, &mut ev);
            return ev.out;
        }

        if let Some(idx) = issuer_in {
            self.issuer_op(ctx, owners, idx, (pot_in, reserve_in, &vault_in), tx, &mut ev);
        } else if let Some(&(vidx, vop)) = vault_in.first() {
            if vault_in.len() > 1 || vidx != 0 {
                // Every vault arm pins its vault at input 0, one per transaction.
                for &(_, op) in &vault_in {
                    self.mark_lost(op, ev.txid, "vault spent outside any covenant layout", &mut ev);
                }
                self.follow_singletons(ctx, pot_in, reserve_in, tx, &mut ev);
            } else {
                self.vault_op(ctx, vop, (pot_in, reserve_in), tx, &mut ev);
            }
        } else {
            // Pot or reserve spent with no issuer and no vault: the permissionless grow-only
            // arms (donations).
            if pot_in.is_some() {
                if let Some((_, v)) = self.follow_pot(ctx, tx, &mut ev) {
                    ev.push(Event::PotGrew { value: v });
                }
            }
            if reserve_in.is_some() {
                if let Some((_, v)) = self.follow_reserve(ctx, tx, &mut ev) {
                    ev.push(Event::ReserveGrew { value: v });
                }
            }
        }
        ev.out
    }

    // --- issuer-gated ops ------------------------------------------------------

    /// Any spend of the issuer token. The successor spk commits the TICK height, for which
    /// nLockTime is only an upper bound (`check_lock_height` is a lower bound on the tx), so
    /// the new anchor is recovered by a descending scan over candidate heights - capped by
    /// the block height, since a minable height-mode locktime never exceeds it and the
    /// covenant forbids timestamp mode (issuer.simf pins height < 500M). The arm is then the
    /// token's successor OUTPUT index, which each covenant arm pins (`recurse_token`): 0 is
    /// POKE, 4 is OPEN, 3 is DRAW or ATTEST - the reserve co-spend tells those apart. The
    /// builder-conventional input positions are verified on top; a mismatch degrades to an
    /// `Anomaly`, never a false record.
    fn issuer_op(
        &mut self,
        ctx: &Ctx,
        owners: &[XOnlyPublicKey],
        idx: usize,
        (pot_in, reserve_in, vault_in): (Option<usize>, Option<usize>, &[(usize, OutPoint)]),
        tx: &Transaction,
        ev: &mut Notices,
    ) {
        let Some(prev) = self.issuer else { return };
        let floor = prev.state.last_mint_height.raw();
        let cap = tx.lock_time.to_consensus_u32().min(ev.height);
        let mut resolved = None;
        let mut h = cap;
        // The anchor is monotone (every arm asserts le_32(last_mint_height, height)), so the
        // scan floor is the previous anchor. The honest case hits on the first iteration
        // (locktime == tick height); an inflated locktime costs at most one derivation per
        // height of slack.
        while h >= floor {
            let state = IssuerState { last_mint_height: BlockHeight::new(h) };
            if let Some((vout, 1)) =
                locate(tx, &ctx.artifacts.issuer_spk(&state), ctx.params.issuer_token)
            {
                resolved = Some((BlockHeight::new(h), vout));
                break;
            }
            if h == 0 {
                break;
            }
            h -= 1;
        }
        let Some((anchor, vout)) = resolved else {
            // The covenant cannot destroy or duplicate the token; if no candidate height
            // derives the successor, our picture of the issuer diverged.
            self.issuer = None;
            ev.push(Event::Anomaly { what: "issuer successor not found at any candidate anchor" });
            return;
        };
        self.issuer = Some(OnChain {
            state: IssuerState { last_mint_height: anchor },
            outpoint: OutPoint::new(ev.txid, vout),
            value: 1,
        });

        match vout {
            // POKE recurses the token to output 0 and is covenant-pinned to input 0.
            0 => {
                if idx != 0 {
                    ev.push(Event::Anomaly { what: "POKE-shaped issuer spend off input 0" });
                    self.follow_singletons(ctx, pot_in, reserve_in, tx, ev);
                    return;
                }
                ev.push(Event::Poked { anchor });
            }
            // OPEN sends the token to output 4; the pot sits immediately before the token
            // (current_index - 1) and the reserve at input 3, both covenant-read positions.
            // The vault is born at output 0.
            4 => {
                if idx == 0 || pot_in != Some(idx - 1) || reserve_in != Some(3) {
                    ev.push(Event::Anomaly {
                        what: "OPEN-shaped issuer spend without pot/reserve pins",
                    });
                    self.follow_singletons(ctx, pot_in, reserve_in, tx, ev);
                    return;
                }
                let Some(minted) = self.pot_delta_shrink(ctx, tx, ev) else { return };
                self.follow_reserve(ctx, tx, ev);
                let birth = Birth { issuer_input: idx, debt: minted, last_height: anchor };
                self.vault_born(ctx, owners, birth, tx, ev);
            }
            // DRAW and ATTEST both send the token to output 3; only ATTEST co-spends the
            // reserve (its bad-debt arm), and it is covenant-pinned to input 4.
            3 if reserve_in.is_none() => {
                if vault_in.first().map(|&(i, _)| i) != Some(0) || idx == 0 || pot_in != Some(idx - 1) {
                    ev.push(Event::Anomaly { what: "DRAW-shaped issuer spend without vault/pot pins" });
                    self.follow_singletons(ctx, pot_in, reserve_in, tx, ev);
                    return;
                }
                let vop = vault_in[0].1;
                let Some(amount) = self.pot_delta_shrink(ctx, tx, ev) else { return };
                let Some(v) = self.vaults.remove(&vop) else { return };
                let debt = Obol::new(v.debt.raw().saturating_add(amount.raw()));
                self.vault_successor(
                    ctx,
                    vop,
                    TrackedVault { debt, last_height: anchor, ..v },
                    tx,
                    ev,
                    |successor| Event::Drawn { vault: vop, successor, amount },
                );
            }
            // BAD-DEBT ATTEST: inputs [vault(0), pot(1), keeper(2), reserve(3), issuer(4), fee(5)].
            3 => {
                if idx != 4
                    || vault_in.first().map(|&(i, _)| i) != Some(0)
                    || pot_in != Some(1)
                    || reserve_in != Some(3)
                {
                    ev.push(Event::Anomaly { what: "ATTEST-shaped issuer spend without pins" });
                    self.follow_singletons(ctx, pot_in, reserve_in, tx, ev);
                    return;
                }
                let vop = vault_in[0].1;
                self.follow_pot(ctx, tx, ev);
                self.follow_reserve(ctx, tx, ev);
                self.vaults.remove(&vop);
                ev.push(Event::BadDebtClosed { vault: vop });
            }
            _ => {
                ev.push(Event::Anomaly { what: "issuer token at an unpinned successor output" });
                self.follow_singletons(ctx, pot_in, reserve_in, tx, ev);
            }
        }
    }

    // --- vault ops without the issuer -------------------------------------------

    /// A tracked vault spent at input 0 with no issuer: REFRESH / REPAY / CLOSE /
    /// LIQUIDATE / REDEEM / FULL-LIQ, told apart by input count and the reserve pin.
    fn vault_op(
        &mut self,
        ctx: &Ctx,
        vop: OutPoint,
        (pot_in, reserve_in): (Option<usize>, Option<usize>),
        tx: &Transaction,
        ev: &mut Notices,
    ) {
        let Some(v) = self.vaults.get(&vop).cloned() else { return };
        match (tx.input.len(), pot_in, reserve_in) {
            // REFRESH: inputs [vault(0), fee coin(1)]. The successor commits the TICK
            // height, which nLockTime only bounds from above; with a resolved owner the
            // exact height is recovered by scanning candidates against output 0's spk
            // (strict ratchet: candidates above the old last_height). For an opaque vault
            // there is no spk to match, so the recorded last_height is an upper bound.
            (2, None, None) => {
                let cap = tx.lock_time.to_consensus_u32().min(ev.height);
                self.vaults.remove(&vop);
                let last_height = match (v.owner, tx.output.first()) {
                    (Some(owner), Some(out0)) => {
                        let found = (v.last_height.raw().saturating_add(1)..=cap).rev().find(|&h| {
                            let state =
                                VaultState { debt: v.debt, owner, last_height: BlockHeight::new(h) };
                            ctx.artifacts.vault_spk(&state) == out0.script_pubkey
                        });
                        let Some(h) = found else {
                            self.mark_lost(vop, ev.txid, "successor spk mismatch", ev);
                            return;
                        };
                        BlockHeight::new(h)
                    }
                    _ => BlockHeight::new(cap),
                };
                self.vault_successor(ctx, vop, TrackedVault { last_height, ..v }, tx, ev, |successor| {
                    Event::Refreshed { vault: vop, successor, last_height }
                });
            }
            // CLOSE: inputs [vault(0), pot(1), payer(2)]; the vault ends, the pot grows by
            // the full debt.
            (3, Some(1), None) => {
                self.follow_pot(ctx, tx, ev);
                self.vaults.remove(&vop);
                ev.push(Event::Closed { vault: vop });
            }
            // REPAY: inputs [vault(0), pot(1), payer(2), fee coin(3)].
            (4, Some(1), None) => {
                let Some(amount) = self.pot_delta_grow(ctx, tx, ev) else {
                    self.mark_lost(vop, ev.txid, "REPAY-shaped spend without a pot successor", ev);
                    return;
                };
                let debt = Obol::new(v.debt.raw().saturating_sub(amount.raw()));
                self.vaults.remove(&vop);
                self.vault_successor(ctx, vop, TrackedVault { debt, ..v }, tx, ev, |successor| {
                    Event::Repaid { vault: vop, successor, amount }
                });
            }
            // LIQUIDATE / REDEEM / FULL-LIQ: inputs [vault(0), pot(1), obol payer(2),
            // reserve(3)]; the pot grows by the repaid amount.
            (4, Some(1), Some(3)) => {
                let Some(amount) = self.pot_delta_grow(ctx, tx, ev) else {
                    self.mark_lost(vop, ev.txid, "liq-shaped spend without a pot successor", ev);
                    return;
                };
                self.follow_reserve(ctx, tx, ev);
                self.vaults.remove(&vop);
                let debt = Obol::new(v.debt.raw().saturating_sub(amount.raw()));
                // A successor keeps the owner and last_height (LIQUIDATE and REDEEM both
                // recurse without touching the ratchet); full-liq has none. With a resolved
                // owner the derived spk decides; opaque vaults fall back to "did the debt
                // survive" (amount == debt closes it - a full-debt REDEEM husk of an opaque
                // vault is indistinguishable from a full-liq by layout, and worthless to us
                // either way).
                let removed = if let Some(owner) = v.owner {
                    let successor = VaultState { debt, owner, last_height: v.last_height };
                    !spk_at_output0(tx, &ctx.artifacts.vault_spk(&successor), ctx.params.policy)
                } else {
                    amount == v.debt
                };
                if removed {
                    if amount != v.debt {
                        self.mark_lost(
                            vop,
                            ev.txid,
                            "liq-family spend with no successor and a surviving debt",
                            ev,
                        );
                        return;
                    }
                    ev.push(Event::FullyLiquidated { vault: vop });
                } else {
                    self.vault_successor(ctx, vop, TrackedVault { debt, ..v }, tx, ev, |successor| {
                        Event::Deleveraged { vault: vop, successor, amount }
                    });
                }
            }
            _ => {
                self.mark_lost(vop, ev.txid, "vault spent outside any covenant layout", ev);
                self.follow_singletons(ctx, pot_in, reserve_in, tx, ev);
            }
        }
    }

    // --- births and successors ---------------------------------------------------

    /// The vault born by an OPEN: output 0, debt = the pot delta, last_height = the
    /// resolved anchor. Owner resolution is candidate-based and trustless either way: a
    /// candidate only counts if `vault_spk(debt, candidate, last_height)` equals the actual
    /// output spk. Candidates come from the configured keys (the wallet's fast path) and,
    /// failing that, from a sliding-window search of the OPEN's issuer-input witness - the
    /// owner travels there as a witness value, and the address commitment judges the
    /// extraction, so no Simplicity decoding is needed. No match leaves the vault opaque.
    fn vault_born(
        &mut self,
        ctx: &Ctx,
        owners: &[XOnlyPublicKey],
        birth: Birth,
        tx: &Transaction,
        ev: &mut Notices,
    ) {
        let Birth { issuer_input, debt, last_height } = birth;
        let Some(out0) = tx.output.first() else {
            ev.push(Event::Anomaly { what: "OPEN without outputs" });
            return;
        };
        let Some(value) = explicit(out0, ctx.params.policy) else {
            ev.push(Event::Anomaly { what: "OPEN vault output not explicit policy" });
            return;
        };
        let matches = |cand: XOnlyPublicKey| {
            let state = VaultState { debt, owner: cand, last_height };
            ctx.artifacts.vault_spk(&state) == out0.script_pubkey
        };
        let owner = owners
            .iter()
            .copied()
            .find(|&cand| matches(cand))
            .or_else(|| scan_witness_for_owner(tx, issuer_input, matches));
        let vault = OutPoint::new(ev.txid, 0);
        self.vaults.insert(
            vault,
            TrackedVault {
                debt,
                last_height,
                value: Sats::new(value),
                owner,
                spk: out0.script_pubkey.clone(),
            },
        );
        ev.push(Event::Opened { vault, debt, value: Sats::new(value), last_height, owner });
    }

    /// Install a vault successor at output 0. For a resolved owner the derived spk must
    /// match the output - a mismatch means the spend was not the transition the layout
    /// claimed, so the vault is lost, not mistracked.
    fn vault_successor(
        &mut self,
        ctx: &Ctx,
        vop: OutPoint,
        next: TrackedVault,
        tx: &Transaction,
        ev: &mut Notices,
        event: impl FnOnce(OutPoint) -> Event,
    ) {
        let Some(out0) = tx.output.first() else {
            self.mark_lost(vop, ev.txid, "vault transition without outputs", ev);
            return;
        };
        let Some(value) = explicit(out0, ctx.params.policy) else {
            self.mark_lost(vop, ev.txid, "vault successor not explicit policy", ev);
            return;
        };
        if let Some(owner) = next.owner {
            let state = VaultState { debt: next.debt, owner, last_height: next.last_height };
            if ctx.artifacts.vault_spk(&state) != out0.script_pubkey {
                self.mark_lost(vop, ev.txid, "successor spk mismatch", ev);
                return;
            }
        }
        let successor = OutPoint::new(ev.txid, 0);
        self.vaults.insert(
            successor,
            TrackedVault { value: Sats::new(value), spk: out0.script_pubkey.clone(), ..next },
        );
        ev.push(event(successor));
    }

    fn mark_lost(&mut self, vop: OutPoint, txid: Txid, reason: &'static str, ev: &mut Notices) {
        self.vaults.remove(&vop);
        self.lost.insert(vop, Lost { txid, reason: reason.to_string() });
        ev.push(Event::VaultLost { vault: vop, reason });
    }

    // --- singleton following -------------------------------------------------------

    /// The pot successor (pinned at output 1 in every pot-spending op, located by spk +
    /// asset). Returns the new outpoint and value. A second same-asset output at the
    /// constant address is possible (a donation smuggled into the op) and reported as a
    /// fragment; the first match is the covenant-pinned one - every decoy-earlier slot
    /// (vault, keeper payout, freed collateral) carries the other asset.
    fn follow_pot(&mut self, ctx: &Ctx, tx: &Transaction, ev: &mut Notices) -> Option<(OutPoint, Obol)> {
        match locate_counted(tx, &ctx.artifacts.pot_spk(), ctx.params.obol) {
            Some((vout, value, extra)) => {
                if extra {
                    ev.push(Event::Fragment { role: "pot" });
                }
                let op = OutPoint::new(ev.txid, vout);
                self.pot = Some(OnChain { state: PotState, outpoint: op, value: Obol::new(value) });
                Some((op, Obol::new(value)))
            }
            None => {
                self.pot = None;
                ev.push(Event::Anomaly { what: "pot spent without a successor at its spk" });
                None
            }
        }
    }

    fn follow_reserve(
        &mut self,
        ctx: &Ctx,
        tx: &Transaction,
        ev: &mut Notices,
    ) -> Option<(OutPoint, Sats)> {
        match locate_counted(tx, &ctx.artifacts.stability_spk(), ctx.params.policy) {
            Some((vout, value, extra)) => {
                if extra {
                    ev.push(Event::Fragment { role: "reserve" });
                }
                let op = OutPoint::new(ev.txid, vout);
                self.reserve =
                    Some(OnChain { state: ReserveState, outpoint: op, value: Sats::new(value) });
                Some((op, Sats::new(value)))
            }
            None => {
                self.reserve = None;
                ev.push(Event::Anomaly { what: "reserve spent without a successor at its spk" });
                None
            }
        }
    }

    /// Best-effort recovery on an unclassified layout: whatever singletons the transaction
    /// spent, follow them to their constant addresses (the covenants cannot move them
    /// elsewhere).
    fn follow_singletons(
        &mut self,
        ctx: &Ctx,
        pot_in: Option<usize>,
        reserve_in: Option<usize>,
        tx: &Transaction,
        ev: &mut Notices,
    ) {
        if pot_in.is_some() {
            self.follow_pot(ctx, tx, ev);
        }
        if reserve_in.is_some() {
            self.follow_reserve(ctx, tx, ev);
        }
    }

    /// The pot delta for minting ops (OPEN / DRAW): old value minus the successor's.
    fn pot_delta_shrink(&mut self, ctx: &Ctx, tx: &Transaction, ev: &mut Notices) -> Option<Obol> {
        let old = self.pot?.value;
        let (_, new) = self.follow_pot(ctx, tx, ev)?;
        match old.checked_sub(new) {
            Ok(d) if d > Obol::ZERO => Some(d),
            _ => {
                ev.push(Event::Anomaly { what: "mint op without a pot shrink" });
                None
            }
        }
    }

    /// The pot delta for repayment ops (REPAY / CLOSE / liq family): the successor's value
    /// minus the old.
    fn pot_delta_grow(&mut self, ctx: &Ctx, tx: &Transaction, ev: &mut Notices) -> Option<Obol> {
        let old = self.pot?.value;
        let (_, new) = self.follow_pot(ctx, tx, ev)?;
        match new.checked_sub(old) {
            Ok(d) if d > Obol::ZERO => Some(d),
            _ => {
                ev.push(Event::Anomaly { what: "repayment op without a pot grow" });
                None
            }
        }
    }

    // --- non-protocol transactions ---------------------------------------------------

    /// No protocol input spent: watch for the ceremony births, and for same-asset payments
    /// into the constant addresses (the L-3 fragmentation threat). Foreign-asset dust never
    /// matches the (spk, asset) filters and is ignored outright.
    fn non_protocol_tx(&mut self, ctx: &Ctx, tx: &Transaction, ev: &mut Notices) {
        if self.pot.is_none() {
            if let Some((vout, value)) = locate(tx, &ctx.artifacts.pot_spk(), ctx.params.obol) {
                let outpoint = OutPoint::new(ev.txid, vout);
                self.pot = Some(OnChain { state: PotState, outpoint, value: Obol::new(value) });
                ev.push(Event::PotBorn { outpoint, value: Obol::new(value) });
            }
        } else if locate(tx, &ctx.artifacts.pot_spk(), ctx.params.obol).is_some() {
            ev.push(Event::Fragment { role: "pot" });
        }

        if self.reserve.is_none() {
            if let Some((vout, value)) = locate(tx, &ctx.artifacts.stability_spk(), ctx.params.policy) {
                let outpoint = OutPoint::new(ev.txid, vout);
                self.reserve = Some(OnChain { state: ReserveState, outpoint, value: Sats::new(value) });
                ev.push(Event::ReserveBorn { outpoint, value: Sats::new(value) });
            }
        } else if locate(tx, &ctx.artifacts.stability_spk(), ctx.params.policy).is_some() {
            ev.push(Event::Fragment { role: "reserve" });
        }

        if let Some(issuer) = self.issuer {
            if locate(tx, &ctx.artifacts.issuer_spk(&issuer.state), ctx.params.issuer_token).is_some() {
                ev.push(Event::Fragment { role: "issuer" });
            }
        } else {
            self.issuer_birth(ctx, ev.height, tx, ev);
        }
    }

    /// The issuance that births the issuer token: a 1-unit output whose spk matches the
    /// anchor of some height at or below the current one (the ceremony anchors at its own
    /// height, so the scan is short and bounded).
    fn issuer_birth(&mut self, ctx: &Ctx, height: u32, tx: &Transaction, ev: &mut Notices) {
        for (vout, out) in tx.output.iter().enumerate() {
            if explicit(out, ctx.params.issuer_token) != Some(1) {
                continue;
            }
            for h in (0..=height).rev() {
                let state = IssuerState { last_mint_height: BlockHeight::new(h) };
                if ctx.artifacts.issuer_spk(&state) == out.script_pubkey {
                    let outpoint = OutPoint::new(ev.txid, vout as u32);
                    self.issuer = Some(OnChain { state, outpoint, value: 1 });
                    ev.push(Event::IssuerBorn { outpoint, anchor: BlockHeight::new(h) });
                    return;
                }
            }
            ev.push(Event::Anomaly { what: "issuer token issued at an underivable spk" });
        }
    }
}

// --- layout helpers -----------------------------------------------------------------

/// What an OPEN commits about its newborn vault, as inferred from the layout.
struct Birth {
    issuer_input: usize,
    debt: Obol,
    last_height: BlockHeight,
}

struct Notices {
    height: u32,
    txid: Txid,
    out: Vec<Notice>,
}

impl Notices {
    fn push(&mut self, event: Event) {
        self.out.push(Notice { height: self.height, txid: self.txid, event });
    }
}

fn input_index(tx: &Transaction, outpoint: OutPoint) -> Option<usize> {
    tx.input.iter().position(|i| i.previous_output == outpoint)
}

/// The explicit value of an output iff it pays the given asset.
fn explicit(out: &TxOut, asset: AssetId) -> Option<u64> {
    match (out.asset, out.value) {
        (CAsset::Explicit(a), CValue::Explicit(v)) if a == asset => Some(v),
        _ => None,
    }
}

/// The first output paying (spk, asset). The asset filter is load-bearing: foreign-asset
/// dust at a protocol address must never look like protocol state.
fn locate(tx: &Transaction, spk: &Script, asset: AssetId) -> Option<(u32, u64)> {
    locate_counted(tx, spk, asset).map(|(vout, value, _)| (vout, value))
}

/// `locate`, plus whether more than one output matched (a fragment rode along).
fn locate_counted(tx: &Transaction, spk: &Script, asset: AssetId) -> Option<(u32, u64, bool)> {
    let mut hits = tx.output.iter().enumerate().filter_map(|(vout, o)| {
        (o.script_pubkey == *spk).then(|| explicit(o, asset).map(|v| (vout as u32, v)))?
    });
    let (vout, value) = hits.next()?;
    Some((vout, value, hits.next().is_some()))
}

/// Does output 0 pay the given spk with the given asset?
fn spk_at_output0(tx: &Transaction, spk: &Script, asset: AssetId) -> bool {
    tx.output.first().is_some_and(|o| o.script_pubkey == *spk && explicit(o, asset).is_some())
}

/// Witness stack elements longer than this are skipped by the owner scan: the issuer's
/// witness-values element (where the owner travels) is a few hundred bytes with a shape
/// fixed by the covenant arm; the multi-kilobyte elements are the program and the control
/// data, which cannot carry witness values. The cap bounds the scan, it does not gate
/// correctness - a miss just leaves the vault opaque.
const OWNER_SCAN_MAX_ELEMENT: usize = 4096;

/// Recover a vault owner from the OPEN's issuer-input witness: slide a 32-byte window over
/// every (small) witness stack element at bit granularity - Simplicity witness values are
/// bit-packed, so the owner word need not be byte-aligned - and accept the first window
/// that derives the vault's actual spk. The address commitment is the judge, exactly as
/// with configured candidates, so the extraction cannot be wrong, only missing.
fn scan_witness_for_owner(
    tx: &Transaction,
    input: usize,
    matches: impl Fn(XOnlyPublicKey) -> bool,
) -> Option<XOnlyPublicKey> {
    let witness = &tx.input.get(input)?.witness.script_witness;
    for element in witness.iter().filter(|e| e.len() >= 32 && e.len() <= OWNER_SCAN_MAX_ELEMENT) {
        for bit in 0..=(element.len() * 8 - 256) {
            let (byte, shift) = (bit / 8, (bit % 8) as u32);
            let mut w = [0u8; 32];
            for (i, out) in w.iter_mut().enumerate() {
                let hi = element[byte + i] << shift;
                let lo = if shift == 0 {
                    0
                } else {
                    element.get(byte + i + 1).copied().unwrap_or(0) >> (8 - shift)
                };
                *out = hi | lo;
            }
            // Roughly half of all windows are not valid x-only points; parsing is much
            // cheaper than the taproot derivation inside `matches`, so it goes first.
            if let Ok(pk) = XOnlyPublicKey::from_slice(&w) {
                if matches(pk) {
                    return Some(pk);
                }
            }
        }
    }
    None
}
