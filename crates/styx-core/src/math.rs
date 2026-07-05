//! Covenant CR math, ported with exact truncation parity.
//!
//! `coll_at_cr` is duplicated between this crate and the frozen vault covenant (vault.simf
//! line 135). The two must truncate identically, or a builder emits an amount one sat off a
//! covenant `<=` / `<` gate. This file mirrors the covenant's jet semantics operation by
//! operation; the property-test tier (M9) additionally proves parity against a verbatim
//! covenant shim.
//!
//! Covenant source (vault.simf):
//! ```text
//! fn coll_at_cr(debt_cents: u32, price: u32, k: u32) -> u64 {
//!     let numerator: u64 = jet::multiply_32(debt_cents, k);   // exact 32x32 -> 64
//!     let denom: u64 = jet::multiply_32(price, 200);
//!     jet::divide_64(numerator, denom)                        // truncating; x/0 = 0
//! }
//! ```
//! All three inputs are u32, so the 32x32 -> 64 products are exact by construction - no
//! overflow is possible anywhere in the covenant's formula. The Rust port therefore takes the
//! same u32 domain; `Obol::covenant_cents()` is the checked narrowing from the u64 amount
//! world (the covenants assert debt < 2^32 at every u64-to-cents narrowing: vault.simf:251,
//! 301, 421; issuer.simf:255, 426).

use crate::units::{Obol, Price, RatioK, Sats};

/// Collateral sats worth `debt_cents` of debt at collateral ratio CR = k / 2_000_000 at
/// `price` USD/BTC. Exact port of the covenant's `coll_at_cr`: truncating division, and a
/// zero denominator (price == 0) yields 0, mirroring `jet::divide_64`.
///
/// k examples: 200_000_000 = 100% (par), 230_000_000 = 115%, 260_000_000 = 130%,
/// 264_000_000 = 132%, 274_000_000 = 137%, 300_000_000 = 150%. Sub-percent k values price
/// fees: 1_000_000 = 0.5% of debt, 10_000_000 = 5%, 40_000_000 = 20%.
pub fn coll_at_cr(debt_cents: u32, price: Price, k: RatioK) -> Sats {
    let numerator = (debt_cents as u64) * (k.raw() as u64); // exact: 32x32 -> 64
    let denom = (price.raw() as u64) * 200; // exact: 32x32 -> 64
    if denom == 0 {
        return Sats::ZERO; // mirror jet::divide_64: division by zero yields 0
    }
    Sats::new(numerator / denom)
}

/// `coll_at_cr` from an `Obol` amount, with the checked narrowing to the covenant's
/// u32-cents domain.
pub fn coll_at_cr_obol(debt: Obol, price: Price, k: RatioK) -> Result<Sats, crate::units::MathError> {
    Ok(coll_at_cr(debt.covenant_cents()?, price, k))
}

/// The reserve's bad-debt payout: min(shortfall + 5% bounty, the 20% per-vault cap (M-1),
/// the reserve balance) - the reserve pays what it can, partially if it must (E-2).
/// Port of the issuer attest arm's sizing (issuer.simf:451-471).
pub fn bad_debt_reserve_pay(debt_cents: u32, price: Price, coll: Sats, reserve: Sats) -> Sats {
    let debt_sats = coll_at_cr(debt_cents, price, crate::consts::K_PAR);
    let shortfall = debt_sats.raw().saturating_sub(coll.raw());
    let bounty = coll_at_cr(debt_cents, price, crate::consts::K_RESERVE_SHARE).raw();
    let cap = coll_at_cr(debt_cents, price, crate::consts::K_BAD_DEBT_CAP).raw();
    // saturating_add where the covenant asserts on overflow: both terms are coll_at_cr
    // results over the u32 debt domain (< 2^33 each), so the sum cannot saturate; the two
    // behaviours coincide on the reachable domain.
    Sats::new(shortfall.saturating_add(bounty).min(cap).min(reserve.raw()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::consts::*;
    use crate::units::MathError;

    #[test]
    fn coll_at_cr_matches_known_vectors() {
        // The reference numbers: $50k debt at $120k/BTC.
        // 150%: (5_000_000 * 300_000_000) / (120_000 * 200) = 62_500_000 sats = 0.625 BTC.
        assert_eq!(
            coll_at_cr(5_000_000, Price::new(120_000), RatioK::from_cr_percent(150)),
            Sats::new(62_500_000)
        );
        // Par (100%) for the same debt: exactly the debt's dollar value in sats.
        assert_eq!(
            coll_at_cr(5_000_000, Price::new(120_000), K_PAR),
            Sats::new(41_666_666) // 50_000 / 120_000 BTC, truncated from ...666.67
        );
        // The 0.5% fee k: fee on a $50k open at $120k/BTC.
        assert_eq!(
            coll_at_cr(5_000_000, Price::new(120_000), K_FEE_HALF_PERCENT),
            Sats::new(208_333) // 0.5% of 41_666_666.67
        );
    }

    #[test]
    fn coll_at_cr_truncates_toward_zero() {
        // 3 * 1_000_003 = 3_000_009; 7 * 200 = 1400; 3_000_009 / 1400 = 2142.86...
        let got = coll_at_cr(3, Price::new(7), RatioK::new(1_000_003));
        assert_eq!(got, Sats::new(2142));
        // nonzero remainder: floor and round differ on this vector
        assert_ne!(2142u64 * 1400, 3_000_009);
    }

    #[test]
    fn coll_at_cr_zero_price_is_zero() {
        // Mirror of the covenant's divide_64(n, 0) = 0. The quorum layer rejects zero-price
        // ticks separately; this identity keeps the two formulas bit-identical on the full
        // input domain.
        assert_eq!(coll_at_cr(5_000_000, Price::new(0), K_PAR), Sats::ZERO);
    }

    #[test]
    fn coll_at_cr_domain_corners_no_overflow() {
        // (2^32 - 1)^2 = 2^64 - 2^33 + 1 fits u64: the covenant's multiply_32 is exact and so
        // is the port. No panic, no wrap, at any corner of the u32 domain.
        let max = u32::MAX;
        assert_eq!(
            coll_at_cr(max, Price::new(1), RatioK::new(max)),
            Sats::new(((max as u64) * (max as u64)) / 200)
        );
        assert_eq!(coll_at_cr(max, Price::new(max), RatioK::new(max)), Sats::new(21_474_836)); // ~ max/200
        assert_eq!(coll_at_cr(0, Price::new(1), RatioK::new(1)), Sats::ZERO);
    }

    #[test]
    fn coll_at_cr_obol_rejects_debt_above_u32() {
        assert_eq!(
            coll_at_cr_obol(Obol::new(5_000_000), Price::new(120_000), RatioK::from_cr_percent(150)),
            Ok(Sats::new(62_500_000))
        );
        assert_eq!(
            coll_at_cr_obol(Obol::new(1 << 32), Price::new(120_000), K_PAR),
            Err(MathError::DebtExceedsU32(1 << 32))
        );
    }

    /// Assert `k` appears as a call-site argument (`, {k})`) in the covenant source. The
    /// leading comma keeps a longer constant from matching on a substring.
    fn assert_gates_on(source: &str, covenant: &str, k: crate::units::RatioK) {
        let needle = format!(", {})", k.raw());
        assert!(
            source.contains(&needle),
            "k literal {} not found as a call-site argument in {covenant}",
            k.raw()
        );
    }

    #[test]
    fn band_constants_match_the_frozen_covenant() {
        // Exact values first, then each literal greps out of the embedded frozen source at
        // its call sites. M9's verbatim shim adds the full semantic parity check.
        assert_eq!(K_PAR.raw(), 200_000_000);
        assert_eq!(K_FULL_LIQ_CAP.raw(), 230_000_000);
        assert_eq!(K_HEALTH_GATE.raw(), 260_000_000);
        assert_eq!(K_HEAL_LO.raw(), 264_000_000);
        assert_eq!(K_HEAL_HI.raw(), 274_000_000);
        assert_eq!(K_OPEN_MIN.raw(), 300_000_000);
        assert_eq!(K_FEE_HALF_PERCENT.raw(), 1_000_000);
        assert_eq!(K_RESERVE_SHARE.raw(), 10_000_000);
        assert_eq!(K_BAD_DEBT_CAP.raw(), 40_000_000);

        let vault = crate::artifacts::Covenant::Vault.source();
        let issuer = crate::artifacts::Covenant::Issuer.source();
        assert_gates_on(vault, "vault", K_PAR); // :377, :402
        assert_gates_on(vault, "vault", K_FULL_LIQ_CAP); // :353, :379
        assert_gates_on(vault, "vault", K_HEALTH_GATE); // :332, :458
        assert_gates_on(vault, "vault", K_HEAL_LO); // :346
        assert_gates_on(vault, "vault", K_HEAL_HI); // :347
        assert_gates_on(vault, "vault", K_OPEN_MIN); // :305 (DRAW)
        assert_gates_on(issuer, "issuer", K_OPEN_MIN); // :187 (OPEN)
        assert_gates_on(vault, "vault", K_FEE_HALF_PERCENT); // :442 (redeem fee)
        assert_gates_on(issuer, "issuer", K_FEE_HALF_PERCENT); // :257 (borrow fee)
        assert_gates_on(vault, "vault", K_RESERVE_SHARE); // :357 (penalty share)
        assert_gates_on(issuer, "issuer", K_RESERVE_SHARE); // :451 (bad-debt bounty)
        assert_gates_on(issuer, "issuer", K_BAD_DEBT_CAP); // :471 (M-1 cap)
    }
}
