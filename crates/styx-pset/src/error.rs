//! Builder and finalization errors.

use styx_core::units::{BlockHeight, MathError, Obol, Sats};

/// The builder refused to construct a transaction the covenants would reject. Every variant
/// carries the numbers, so the caller sees the gate and the margin instead of a prune failure
/// later.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The tick is older than the issuer's mint-recency anchor (non-strict floor).
    #[error("stale tick: height {} is below the issuer anchor {}", tick.raw(), anchor.raw())]
    StaleTick { tick: BlockHeight, anchor: BlockHeight },
    /// The tick does not advance the vault's freshness ratchet, which is strict: DRAW and
    /// REFRESH need a tick strictly newer than the vault's last_height.
    #[error("tick height {} does not advance the vault ratchet at {}", tick.raw(), last_height.raw())]
    RatchetNotAdvanced { tick: BlockHeight, last_height: BlockHeight },
    /// A quote carries a zero price; the covenants reject it at the quorum layer.
    #[error("zero price in the oracle tick")]
    ZeroPrice,
    /// A zero op amount (open principal, draw amount, liquidate dd). The covenants assert
    /// each is positive: a zero-principal OPEN would let the pot be spent through the inflow
    /// leaf without the token gate (issuer.simf:264), and the vault's partial arm requires
    /// 0 < dd (vault.simf:324).
    #[error("zero amount")]
    ZeroAmount,
    /// Collateral below the covenant's CR gate for this operation.
    #[error("undercollateralized: need {} sats, have {}", need.raw(), have.raw())]
    Undercollateralized { need: Sats, have: Sats },
    /// The pot cannot release this much OBOL.
    #[error("insufficient pot: need {} OBOL units, have {}", need.raw(), have.raw())]
    InsufficientPot { need: Obol, have: Obol },
    /// The funding coin does not cover the amounts this op must pay (plus a positive change).
    #[error("insufficient funding: need more than {} sats, have {}", need.raw(), have.raw())]
    InsufficientFunding { need: Sats, have: Sats },
    /// The funding coin must cover the op's outputs exactly (no change output in the frozen
    /// layouts).
    #[error("funding mismatch: need exactly {} sats, have {}", need.raw(), have.raw())]
    FundingMismatch { need: Sats, have: Sats },
    /// A partial liquidation must leave a residual debt (vault.simf:327 asserts
    /// 0 < residual_debt); a full repayment is FULL-LIQ's job.
    #[error("dd {} leaves no residual debt (use full-liq for {})", dd.raw(), debt.raw())]
    NotPartial { dd: Obol, debt: Obol },
    /// The vault sits at or above the 130% gate: partial liquidation is closed.
    #[error("vault too healthy: collateral {} is not below the {} sat gate", have.raw(), gate.raw())]
    VaultTooHealthy { gate: Sats, have: Sats },
    /// The residual collateral misses the [132%, 137%] heal band.
    #[error("residual {} outside the heal band [{}, {}]", residual.raw(), lo.raw(), hi.raw())]
    HealOutOfBand { residual: Sats, lo: Sats, hi: Sats },
    /// The keeper extraction exceeds the 1.15 x dd cap.
    #[error("extraction {} exceeds the cap {}", extraction.raw(), cap.raw())]
    ExtractionExceedsCap { extraction: Sats, cap: Sats },
    /// The vault CR is outside this op's band (full-liq needs [100%, 115%]).
    #[error("collateral {} outside the band [{}, {}]", coll.raw(), floor.raw(), cap.raw())]
    CrOutOfBand { coll: Sats, floor: Sats, cap: Sats },
    /// Bad-debt needs CR < 100%: the collateral still covers the debt.
    #[error("not underwater: collateral {} covers the debt's {} sats", coll.raw(), debt_sats.raw())]
    NotUnderwater { debt_sats: Sats, coll: Sats },
    /// The OBOL payer coin cannot cover the repayment.
    #[error("insufficient payer: need {} OBOL units, have {}", need.raw(), have.raw())]
    InsufficientPayer { need: Obol, have: Obol },
    /// A repayment above the vault's debt.
    #[error("amount {} exceeds the debt {}", amount.raw(), debt.raw())]
    AmountExceedsDebt { amount: Obol, debt: Obol },
    #[error(transparent)]
    Math(#[from] MathError),
}

/// A covenant input rejected at prune time. For a builder-produced transaction this is a bug:
/// the builder should have refused first.
#[derive(Debug, thiserror::Error)]
#[error("input {input} ({covenant}) rejected at prune time: {message}")]
pub struct PruneRejected {
    pub input: u32,
    pub covenant: &'static str,
    pub message: String,
}
