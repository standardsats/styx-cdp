//! One module per operation. Each has a checked entry point (refuses what the covenants
//! would reject, with the numbers) and an unchecked constructor used by the negative test
//! tier, where the covenant itself is the judge.

pub mod bad_debt;
pub mod close;
pub mod draw;
pub mod full_liq;
pub mod liquidate;
pub mod open;
pub mod poke;
pub mod refresh;
pub mod repay;

use styx_core::oracle::OracleTick;
use styx_core::units::BlockHeight;

use crate::error::BuildError;

/// The covenants reject a zero price at the quorum layer; builders refuse first. The height
/// threshold is already unrepresentable (`OracleTick::new` rejects it).
pub(crate) fn check_zero_price(tick: &OracleTick) -> Result<(), BuildError> {
    let (lo, _) = tick.price_range();
    if lo.raw() == 0 {
        return Err(BuildError::ZeroPrice);
    }
    Ok(())
}

/// The tick preflight every issuer-gated op shares: no zero price, not below the issuer's
/// mint-recency anchor (non-strict).
pub(crate) fn check_tick(tick: &OracleTick, anchor: BlockHeight) -> Result<(), BuildError> {
    check_zero_price(tick)?;
    if tick.height() < anchor {
        return Err(BuildError::StaleTick { tick: tick.height(), anchor });
    }
    Ok(())
}

/// The vault's freshness ratchet is strict: DRAW and REFRESH need a tick strictly newer than
/// the vault's last_height.
pub(crate) fn check_vault_ratchet(
    tick: &OracleTick,
    last_height: BlockHeight,
) -> Result<(), BuildError> {
    if tick.height() <= last_height {
        return Err(BuildError::RatchetNotAdvanced { tick: tick.height(), last_height });
    }
    Ok(())
}
