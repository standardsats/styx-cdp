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
    /// A zero mint. The issuer forbids it (issuer.simf:264): a zero-principal OPEN would let
    /// the pot be spent through the inflow leaf without the token gate.
    #[error("zero principal")]
    ZeroPrincipal,
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
