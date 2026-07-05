//! Transaction primitives shared by the builders, and the layout conventions.
//!
//! Every op has a fixed input/output index layout the covenants pin (documented per build
//! module). Common to all of them:
//! - every input uses `Sequence::ZERO`, so nLockTime is consensus-enforceable (tick-carrying
//!   ops set nLockTime to the tick height for `check_lock_height`);
//! - every protocol output is explicit (unblinded) - the covenants' explicit-read jets fail
//!   on confidential values;
//! - the fee output is last.

use styx_core::elements::{
    confidential, AssetId, AssetIssuance, OutPoint, Script, Sequence, TxIn, TxInWitness, TxOut,
    TxOutWitness,
};
use styx_core::simplicity::jet::elements::ElementsUtxo;
use styx_core::units::Sats;

pub fn txin(outpoint: OutPoint) -> TxIn {
    TxIn {
        previous_output: outpoint,
        is_pegin: false,
        script_sig: Script::new(),
        sequence: Sequence::ZERO,
        asset_issuance: AssetIssuance::null(),
        witness: TxInWitness::empty(),
    }
}

/// An explicit (unblinded) output.
pub fn txout(value: u64, spk: Script, asset: AssetId) -> TxOut {
    TxOut {
        value: confidential::Value::Explicit(value),
        script_pubkey: spk,
        asset: confidential::Asset::Explicit(asset),
        nonce: confidential::Nonce::Null,
        witness: TxOutWitness::empty(),
    }
}

/// A claimed input UTXO for the spend environment. The env never dereferences outpoints
/// against a chain; prune-tier tests claim synthetic ones.
pub fn claimed(value: u64, spk: Script, asset: AssetId) -> ElementsUtxo {
    ElementsUtxo::from(txout(value, spk, asset))
}

pub fn fee_out(fee: Sats, policy: AssetId) -> TxOut {
    TxOut::new_fee(fee.raw(), policy)
}
