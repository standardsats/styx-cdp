//! The builder output: a complete transaction body plus the plan for witnessing it.

use styx_core::domain::{IssuerState, VaultState};
use styx_core::elements::{Transaction, Txid};
use styx_core::encode::{IssuerOp, StabilityOp, VaultOp};
use styx_core::simplicity::jet::elements::ElementsUtxo;

/// Which covenant satisfies one input, and with what witness values. Key-spend (wallet)
/// inputs are not listed: their signatures are checked by the node, not by a covenant, and
/// are attached at the PSET signing stage.
#[derive(Debug, Clone)]
pub enum SlotKind {
    /// The pot's grow-only inflow leaf (`reserve_repay`). No witness values, no pruning.
    PotInflow,
    /// The pot's issuer-gated outflow leaf. No witness values, no pruning.
    PotOutflow,
    /// The stability reserve, with its two-way OP.
    Stability(StabilityOp),
    /// The issuer singleton: the spent state and the op. Boxed: the op carries a full
    /// oracle tick, and clippy flags the size gap against the payload-free pot variants.
    Issuer { state: IssuerState, op: Box<IssuerOp> },
    /// A vault: the spent state and the op. Owner ops carry their signature as data inside
    /// the op; it is signed over the vault input's sighash, so the vault slot goes last in
    /// the plan and `finalize::vault_sighash` exposes the digest to sign.
    Vault { state: VaultState, op: Box<VaultOp> },
}

#[derive(Debug, Clone)]
pub struct WitnessSlot {
    pub input: u32,
    pub kind: SlotKind,
}

/// A fixed transaction body, the claimed input UTXOs, and the witness plan. The body
/// determines the txid (witnesses are segregated), so successor outpoints are known before
/// finalization.
#[derive(Debug, Clone)]
pub struct TxPlan {
    pub tx: Transaction,
    pub in_utxos: Vec<ElementsUtxo>,
    /// In witnessing order: signature-free covenants first, the vault last (the defensive
    /// convention: its owner-signature slots are filled before covenant pruning).
    pub slots: Vec<WitnessSlot>,
}

impl TxPlan {
    pub fn txid(&self) -> Txid {
        self.tx.txid()
    }

    /// Apply one mutation between build and finalize. The single-cause negative tests are
    /// built from a genuine plan plus exactly one tamper.
    pub fn tamper(mut self, f: impl FnOnce(&mut Transaction, &mut Vec<ElementsUtxo>)) -> Self {
        f(&mut self.tx, &mut self.in_utxos);
        self
    }
}

/// A plan plus the successor state it produces if confirmed. Callers update tracked state
/// from `expected` only after confirmation.
#[derive(Debug, Clone)]
pub struct Built<D> {
    pub plan: TxPlan,
    pub expected: D,
}
