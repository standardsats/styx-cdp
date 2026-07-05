//! The stability reserve's OP sum (stability.simf): accumulate = Left(()), bad-debt = Right(()).
//! Neither arm carries data - the reserve is constant-address, and on bad-debt the issuer
//! authors the amount and the recency.

use super::{Either, ToSimf};
use simplicityhl::Value;

pub type StabilityOpSum = Either<(), ()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StabilityOp {
    /// Grow-only inflow: liquidation penalty shares and redemption fees.
    Accumulate,
    /// Release the issuer-authored shortfall on a bad-debt close.
    BadDebt,
}

pub fn lower_stability_op(op: &StabilityOp) -> StabilityOpSum {
    match op {
        StabilityOp::Accumulate => Either::Left(()),
        StabilityOp::BadDebt => Either::Right(()),
    }
}

pub fn stability_op_value(op: &StabilityOp) -> Value {
    lower_stability_op(op).value()
}
