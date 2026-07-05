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

/// Partial LIQUIDATE: repay `dd`, heal the vault to `residual` collateral (inside the
/// [132%, 137%] band at the max quote), seize the rest less the 5% reserve share and the fee.
#[derive(Debug, Clone)]
pub struct LiquidateIntent {
    pub dd: Obol,
    pub residual: Sats,
    pub keeper: ObolCoin,
    pub keeper_spk: Script,
    pub obol_change_spk: Script,
    pub tick: OracleTick,
    pub fee: Sats,
}

/// FULL-LIQ: repay the full debt in the [100%, 115%] band, seize the collateral less one
/// third of the excess (to the reserve) and the fee. The keeper coin must exceed the debt:
/// the positive OBOL change output is the E-5 anchor.
#[derive(Debug, Clone)]
pub struct FullLiqIntent {
    pub keeper: ObolCoin,
    pub keeper_spk: Script,
    pub obol_change_spk: Script,
    pub tick: OracleTick,
    pub fee: Sats,
}

/// BAD-DEBT (issuer-attested): repay the full debt of an underwater vault (CR < 100%); the
/// reserve covers the shortfall plus the 5% bounty, capped at 20% of the debt (M-1) and at
/// its balance (E-2). The tx fee comes from a separate coin.
#[derive(Debug, Clone)]
pub struct BadDebtIntent {
    pub keeper: ObolCoin,
    pub keeper_spk: Script,
    pub obol_change_spk: Script,
    pub fee_coin: FundingCoin,
    pub change_spk: Script,
    pub tick: OracleTick,
    pub fee: Sats,
}

/// REDEEM `x` OBOL for collateral at the peg floor: valued at the max quote and the backing
/// floor min(par, backing_k), less the 0.5% fee to the reserve. Permissionless, no health
/// gate; x may equal the full debt.
#[derive(Debug, Clone)]
pub struct RedeemIntent {
    pub x: Obol,
    pub redeemer: ObolCoin,
    pub redeemer_spk: Script,
    pub obol_change_spk: Script,
    pub tick: OracleTick,
    pub fee: Sats,
}
