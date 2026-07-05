//! Intents: what the caller wants, separated from how the transaction is laid out.

use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::elements::{OutPoint, Script};
use styx_core::oracle::OracleTick;
use styx_core::units::{Obol, Sats};

/// A plain wallet coin funding an operation. The spk is the claimed UTXO script the spend
/// environment sees; signing it is the wallet's job (M10 wires PSET key-spend signing,
/// SIGHASH_ALL per E-5).
#[derive(Debug, Clone)]
pub struct FundingCoin {
    pub outpoint: OutPoint,
    pub value: Sats,
    pub spk: Script,
}

/// POKE: advance the issuer's mint-recency anchor to `tick`'s height. Permissionless, no
/// token movement; the tx fee comes from `funding`, change returns to `change_spk`.
#[derive(Debug, Clone)]
pub struct PokeIntent {
    pub tick: OracleTick,
    pub funding: FundingCoin,
    pub change_spk: Script,
    pub fee: Sats,
}

/// OPEN: lock `collateral`, mint `principal` OBOL to `borrower_spk`. The vault commits
/// debt = principal; the 0.5% borrow fee is paid in L-BTC to the reserve. `funding` must
/// cover collateral + borrow fee + tx fee exactly - the frozen OPEN layout has no change
/// output.
#[derive(Debug, Clone)]
pub struct OpenIntent {
    pub owner: XOnlyPublicKey,
    pub principal: Obol,
    pub collateral: Sats,
    pub borrower_spk: Script,
    pub funding: FundingCoin,
    pub tick: OracleTick,
    pub fee: Sats,
}
