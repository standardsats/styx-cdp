//! Unit newtypes: every amount in the protocol carries its unit in the type.
//!
//! Putting an OBOL amount into an L-BTC output slot is a compile error. No `Deref` to the raw
//! integer; sums that can exceed the type return `MathError` instead of wrapping.
//!
//! Scales:
//! - `Sats` - L-BTC satoshis (the collateral and reserve asset).
//! - `Obol` - OBOL atomic units, which are also debt cents: 1 OBOL unit == $0.01 of debt.
//!   The covenants' `debt_cents` / `principal` and the pot balance are all this one unit.
//! - `Price` - integer USD per BTC, signed as 4 BE bytes inside an oracle tick.
//! - `RatioK` - the covenant's collateral-ratio unit: k = CR_percent * 2_000_000.
//! - `BlockHeight` - chain height, the freshness ratchet's unit.

/// Arithmetic that left the representable range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MathError {
    #[error("amount overflow")]
    Overflow,
    #[error("amount underflow")]
    Underflow,
    /// A debt amount above the covenant's u32-cents domain. The covenants assert
    /// `debt < 2^32` wherever a u64 amount narrows to cents: vault.simf:251 (witness debt),
    /// :301 (DRAW), :421 (REDEEM x); issuer.simf:255 (OPEN), :426 (ATTEST). A compliant
    /// chain state never trips this.
    #[error("debt {0} does not fit the covenant's u32 cents domain")]
    DebtExceedsU32(u64),
}

macro_rules! amount_newtype {
    ($(#[$doc:meta])* $name:ident($int:ty)) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name($int);

        impl $name {
            pub const ZERO: $name = $name(0);

            pub const fn new(raw: $int) -> Self {
                $name(raw)
            }
            /// The raw integer, for serialization and covenant-witness encoding.
            pub const fn raw(self) -> $int {
                self.0
            }
            pub fn checked_add(self, other: Self) -> Result<Self, MathError> {
                self.0.checked_add(other.0).map($name).ok_or(MathError::Overflow)
            }
            pub fn checked_sub(self, other: Self) -> Result<Self, MathError> {
                self.0.checked_sub(other.0).map($name).ok_or(MathError::Underflow)
            }
        }
    };
}

amount_newtype! {
    /// An L-BTC amount in satoshis.
    Sats(u64)
}
amount_newtype! {
    /// An OBOL amount in atomic units (= debt cents: 1 unit is $0.01 of debt).
    Obol(u64)
}

impl Obol {
    /// Narrow to the covenant's u32-cents domain, mirroring the covenants' own
    /// `assert!(jet::lt_64(debt, 4294967296))` gates (vault.simf:251,301,421;
    /// issuer.simf:255,426). Covenant CR math (`coll_at_cr`) is only defined on this domain.
    pub fn covenant_cents(self) -> Result<u32, MathError> {
        u32::try_from(self.0).map_err(|_| MathError::DebtExceedsU32(self.0))
    }
}

/// An oracle price: integer USD per BTC, signed as 4 BE bytes inside a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Price(u32);

impl Price {
    pub const fn new(usd_per_btc: u32) -> Self {
        Price(usd_per_btc)
    }
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A collateral ratio in the covenant's k units: k = CR_percent * 2_000_000.
/// 200_000_000 = 100% (par), 260_000_000 = 130%, 300_000_000 = 150%.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RatioK(u32);

impl RatioK {
    /// k units per CR percentage point.
    pub const PER_CR_PERCENT: u32 = 2_000_000;

    pub const fn new(k: u32) -> Self {
        RatioK(k)
    }
    /// A whole-percent collateral ratio, e.g. `from_cr_percent(150)` = k 300_000_000.
    /// Domain: percent <= 2147 (u32::MAX / 2_000_000); protocol bands are all <= 150.
    pub const fn from_cr_percent(percent: u32) -> Self {
        debug_assert!(percent <= u32::MAX / Self::PER_CR_PERCENT);
        RatioK(percent * Self::PER_CR_PERCENT)
    }
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A chain height: the unit of the vault's freshness ratchet, the issuer's mint anchor, and
/// oracle tick anchoring (`check_lock_height`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BlockHeight(u32);

impl BlockHeight {
    pub const fn new(height: u32) -> Self {
        BlockHeight(height)
    }
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn checked_add_overflow_is_an_error() {
        assert_eq!(Sats::new(u64::MAX).checked_add(Sats::new(1)), Err(MathError::Overflow));
        assert_eq!(Sats::new(1).checked_add(Sats::new(2)), Ok(Sats::new(3)));
    }

    #[test]
    fn checked_sub_underflow_is_an_error() {
        assert_eq!(Obol::new(1).checked_sub(Obol::new(2)), Err(MathError::Underflow));
        assert_eq!(Obol::new(2).checked_sub(Obol::new(2)), Ok(Obol::ZERO));
    }

    #[test]
    fn covenant_cents_mirrors_the_mint_gate() {
        // The boundary the issuer asserts at mint time: debt < 2^32 cents.
        assert_eq!(Obol::new(u32::MAX as u64).covenant_cents(), Ok(u32::MAX));
        assert_eq!(
            Obol::new(u32::MAX as u64 + 1).covenant_cents(),
            Err(MathError::DebtExceedsU32(u32::MAX as u64 + 1))
        );
    }

    #[test]
    fn ratio_k_percent_convention() {
        assert_eq!(RatioK::from_cr_percent(100).raw(), 200_000_000);
        assert_eq!(RatioK::from_cr_percent(130).raw(), 260_000_000);
        assert_eq!(RatioK::from_cr_percent(150).raw(), 300_000_000);
    }
}
