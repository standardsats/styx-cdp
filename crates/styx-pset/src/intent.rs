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

/// An OBOL-denominated wallet coin (repayments, keeper funding).
#[derive(Debug, Clone)]
pub struct ObolCoin {
    pub outpoint: OutPoint,
    pub value: Obol,
    pub spk: Script,
}

/// CLOSE: repay the full debt, free the collateral to `recipient_spk`. The tx fee comes from
/// the freed collateral; a payer surplus over the debt returns to `payer_change_spk`.
#[derive(Debug, Clone)]
pub struct CloseIntent {
    pub payer: ObolCoin,
    pub recipient_spk: Script,
    pub payer_change_spk: Script,
    pub fee: Sats,
}

/// REPAY `amount`: reduce the debt, collateral preserved in full. The covenant rejects any
/// collateral reduction, so the tx fee MUST come from a separate coin - the field's presence
/// is the policy.
#[derive(Debug, Clone)]
pub struct RepayIntent {
    pub amount: Obol,
    pub payer: ObolCoin,
    pub payer_change_spk: Script,
    pub fee_coin: FundingCoin,
    pub change_spk: Script,
    pub fee: Sats,
}

/// DRAW `amount` more OBOL against the vault. The tx fee comes from the collateral (the
/// covenant re-checks 150% on the post-draw collateral).
#[derive(Debug, Clone)]
pub struct DrawIntent {
    pub amount: Obol,
    pub borrower_spk: Script,
    pub tick: OracleTick,
    pub fee: Sats,
}

/// REFRESH: advance the vault's freshness ratchet, proving CR >= 130% at the max quote.
/// Permissionless; collateral preserved, so the fee comes from a separate coin.
#[derive(Debug, Clone)]
pub struct RefreshIntent {
    pub tick: OracleTick,
    pub fee_coin: FundingCoin,
    pub change_spk: Script,
    pub fee: Sats,
}
