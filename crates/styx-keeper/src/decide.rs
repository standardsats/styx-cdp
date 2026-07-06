//! The pure decision ladder: what one look at (vault, tick) obliges a keeper to do.
//!
//! Bands mirror the covenant gates exactly, all priced at the MAX quote (liquidations and
//! redemptions read `hi`, mints read `lo`):
//!
//!   coll <  100% of debt          -> BAD-DEBT   (strict, bad_debt.rs / issuer ATTEST)
//!   coll in [100%, 115%] of debt  -> FULL-LIQ   (inclusive both ends, full_liq.rs)
//!   coll in (115%, 130%) of debt  -> PARTIAL    (the 130% gate is strict; `plan_partial`
//!                                                sizes the heal)
//!   healthy but stale             -> REFRESH    (M-2: keepers keep dormant vaults
//!                                                liquidatable-current)
//!
//! Everything here is deterministic integer covenant math (`coll_at_cr`); the property
//! tests hold each verdict against the checked builders, which refuse anything the
//! covenants would.

use styx_core::consts::{K_FULL_LIQ_CAP, K_HEALTH_GATE, K_HEAL_HI, K_HEAL_LO, K_PAR, K_RESERVE_SHARE};
use styx_core::domain::{OnChain, VaultState};
use styx_core::math::coll_at_cr;
use styx_core::oracle::OracleTick;
use styx_core::units::{BlockHeight, Obol, Price, Sats};

/// What the keeper should do about one vault, given one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Healthy and fresh enough.
    None,
    /// Healthy but the ratchet lags more than `refresh_lag` behind the tick.
    Refresh,
    /// Partial liquidation: repay `dd`, heal the vault to `residual` collateral.
    Partial {
        dd: Obol,
        residual: Sats,
    },
    FullLiq,
    BadDebt,
}

/// The decision for one vault, complete with the builders' preconditions: a tick at or
/// below the vault's ratchet decides nothing (the strict-ratchet check would refuse every
/// op it could justify), and a bad-debt verdict additionally needs the tick at or above
/// `anchor` - the issuer's mint-recency floor its ATTEST arm enforces (the poke duty keeps
/// the anchor near the tip, so this gate closes itself within a block or two).
pub fn decide(
    vault: &OnChain<VaultState>,
    tick: &OracleTick,
    anchor: BlockHeight,
    refresh_lag: u32,
    fee: Sats,
) -> Action {
    if tick.height() <= vault.state.last_height {
        return Action::None;
    }
    let Ok(debt_cents) = vault.state.debt.covenant_cents() else {
        return Action::None; // covenant-invalid debt cannot exist on a compliant chain
    };
    if debt_cents == 0 {
        return Action::None; // a debtless husk: nothing to liquidate, nothing to protect
    }
    let (_, hi) = tick.price_range();
    let floor = coll_at_cr(debt_cents, hi, K_PAR);
    let cap = coll_at_cr(debt_cents, hi, K_FULL_LIQ_CAP);
    let gate = coll_at_cr(debt_cents, hi, K_HEALTH_GATE);
    if vault.value < floor {
        if tick.height() < anchor {
            return Action::None; // the ATTEST arm would refuse a tick under the anchor
        }
        Action::BadDebt
    } else if vault.value <= cap {
        Action::FullLiq
    } else if vault.value < gate {
        match plan_partial(vault.state.debt, vault.value, hi, fee) {
            Some((dd, residual)) => Action::Partial { dd, residual },
            None => Action::None,
        }
    } else if tick.height().raw() - vault.state.last_height.raw() > refresh_lag {
        Action::Refresh
    } else {
        Action::None
    }
}

/// Size a partial liquidation: the largest `dd` whose maximal extraction still lands the
/// residual inside the heal band, with the residual as low in the band as the extraction
/// cap allows (extraction = coll - residual <= 115% of dd - the owner loses at most
/// 1.15 x dd).
///
/// Feasibility of "extraction at the cap fits under the band ceiling" is monotone in real
/// numbers and quasi-monotone in the integers (see the note at `fits`): the cap grows with
/// dd while the band shrinks, so `coll - cap(dd) <= band_hi(dd)` holds up to some dd and
/// fails beyond - a binary search finds an edge and a linear tail walk claims the jitter.
/// The residual is then clamped to the band floor from below (small dd near the gate needs
/// less than the cap allows). Returns None when no dd yields a builder-acceptable,
/// profitable liquidation.
pub fn plan_partial(debt: Obol, coll: Sats, hi: Price, fee: Sats) -> Option<(Obol, Sats)> {
    let debt_cents = debt.covenant_cents().ok()?;
    if debt_cents < 2 {
        return None; // dd must be in (0, debt): the covenant requires a positive residual debt
    }
    let cap = |dd: u64| coll_at_cr(dd as u32, hi, K_FULL_LIQ_CAP).raw();
    let band = |dd: u64| {
        let rd = (debt.raw() - dd) as u32;
        (coll_at_cr(rd, hi, K_HEAL_LO).raw(), coll_at_cr(rd, hi, K_HEAL_HI).raw())
    };
    // In real numbers cap(dd) + band_hi(dd) falls by (137 - 115)% x 1e6 / price sats per
    // cent of dd, so `fits` is monotone (true then false) - but only quasi-monotone in the
    // integers once that step drops below one satoshi (price > ~220_000 USD/BTC): floor
    // jitter can re-open feasibility just past a local edge. The binary search finds an
    // edge; the linear walk below then claims whatever jitter re-opened.
    let fits = |dd: u64| coll.raw().saturating_sub(cap(dd)) <= band(dd).1;

    let (mut lo, mut hi_dd) = (1u64, debt.raw() - 1);
    if !fits(lo) {
        return None;
    }
    while lo < hi_dd {
        let mid = lo + (hi_dd - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi_dd = mid - 1;
        }
    }
    let mut dd = lo;
    while dd + 1 < debt.raw() && fits(dd + 1) {
        dd += 1;
    }
    let (band_lo, band_hi) = band(dd);
    let residual = band_lo.max(coll.raw().saturating_sub(cap(dd)));
    if residual > band_hi || residual > coll.raw() {
        return None; // the feasible interval is empty (band floor above the collateral)
    }
    let extraction = coll.raw() - residual;
    let share = coll_at_cr(dd as u32, hi, K_RESERVE_SHARE).raw();
    // The builder's profitability refusal: the extraction must cover the reserve share and
    // the tx fee with something left for the keeper.
    if extraction <= share + fee.raw() {
        return None;
    }
    Some((Obol::new(dd), Sats::new(residual)))
}
