//! One module per operation. Each has a checked entry point (refuses what the covenants
//! would reject, with the numbers) and an unchecked constructor used by the negative test
//! tier, where the covenant itself is the judge.

pub mod open;
pub mod poke;

use styx_core::oracle::OracleTick;
use styx_core::units::BlockHeight;

use crate::error::BuildError;

/// The tick preflight every issuer-gated op shares: no zero price, not below the anchor.
/// The height threshold is already unrepresentable (`OracleTick::new` rejects it).
pub(crate) fn check_tick(tick: &OracleTick, anchor: BlockHeight) -> Result<(), BuildError> {
    let (lo, _) = tick.price_range();
    if lo.raw() == 0 {
        return Err(BuildError::ZeroPrice);
    }
    if tick.height() < anchor {
        return Err(BuildError::StaleTick { tick: tick.height(), anchor });
    }
    Ok(())
}
