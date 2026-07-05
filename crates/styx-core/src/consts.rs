//! Protocol constants shared by the taproot layer and the covenant params.
//!
//! Everything here is protocol-level, independent of any particular deploy:
//! the data-leaf version byte, the leaf version-control words the covenants reconstruct
//! Merkle branches with, the canonical NUMS internal key, and the Elements tapleaf tag.

use simplicityhl::elements::hashes::{sha256, Hash};
use simplicityhl::elements::secp256k1_zkp::XOnlyPublicKey;
use std::str::FromStr;

use crate::units::RatioK;
use crate::U256;

// --- collateral-ratio bands and fee rates (the k literals the frozen covenants gate on) ------
//
// All in RatioK units (k = CR_percent * 2_000_000). Line references are into the frozen
// covenant sources at covenants/, which the golden-CMR test pins.

/// 100% - par backing. The redemption floor clamp and the bad-debt boundary
/// (vault.simf:377,402; a tick signed at par makes the E-2 backing floor a no-op).
pub const K_PAR: RatioK = RatioK::from_cr_percent(100);
/// 115% - the full-liquidation band top and the partial-liq extraction cap
/// (owner loses at most 1.15 x dd; vault.simf:353,379).
pub const K_FULL_LIQ_CAP: RatioK = RatioK::from_cr_percent(115);
/// 130% - the health gate: below it partial liquidation opens, at or above it REFRESH
/// proves health (vault.simf:332,458).
pub const K_HEALTH_GATE: RatioK = RatioK::from_cr_percent(130);
/// 132% - the partial-liquidation heal band floor (vault.simf:346).
pub const K_HEAL_LO: RatioK = RatioK::from_cr_percent(132);
/// 137% - the partial-liquidation heal band ceiling (vault.simf:347).
pub const K_HEAL_HI: RatioK = RatioK::from_cr_percent(137);
/// 150% - the minimum CR at OPEN (issuer.simf:187) and DRAW (vault.simf:305).
pub const K_OPEN_MIN: RatioK = RatioK::from_cr_percent(150);
/// 0.5% of debt - the borrow fee at OPEN (issuer.simf:257) and the redemption fee
/// (vault.simf:442), both routed to the stability reserve.
pub const K_FEE_HALF_PERCENT: RatioK = RatioK::new(1_000_000);
/// 5% of debt - the reserve's share of the partial-liq penalty (vault.simf:357) and the
/// bad-debt keeper bounty (issuer.simf:451).
pub const K_RESERVE_SHARE: RatioK = RatioK::new(10_000_000);
/// 20% of debt - the per-vault cap on a bad-debt reserve payout, audit finding M-1
/// (issuer.simf:471).
pub const K_BAD_DEBT_CAP: RatioK = RatioK::new(40_000_000);

/// Taproot leaf version used for the unspendable OP_RETURN data leaves (vault, issuer).
pub const DATA_LEAF_VER: u8 = 0xc4;

/// The canonical BIP341 nothing-up-my-sleeve internal key H (x-only, hex).
/// Every covenant output is finalized to this key, so no key-path spend exists.
pub const INTERNAL_KEY_HEX: &str = "50929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0";

/// The canonical NUMS internal key as an x-only pubkey.
pub fn nums_key() -> XOnlyPublicKey {
    // The derivation test proves this constant is the BIP341 H: sha256 of the uncompressed
    // generator encoding, so its discrete log is unknown and no key-path spend exists.
    #[allow(clippy::unwrap_used)]
    XOnlyPublicKey::from_str(INTERNAL_KEY_HEX).unwrap()
}

/// sha256("TapLeaf/elements") - the tagged-hash prefix the covenants use to recompute
/// tapleaf hashes when reconstructing sibling data leaves.
pub fn tapleaf_tag() -> U256 {
    U256::from_byte_array(sha256::Hash::hash(b"TapLeaf/elements").to_byte_array())
}

/// Version-control word of the 45-byte vault data leaf:
/// version || compact_size(45); script = OP_RETURN || debt(8) || owner(32) || last_height(4).
pub fn leaf_vc() -> u16 {
    ((DATA_LEAF_VER as u16) << 8) | 0x2d
}

/// Version-control word of the 5-byte issuer data leaf:
/// version || compact_size(5); script = OP_RETURN || last_mint_height(4).
pub fn issuer_leaf_vc() -> u16 {
    ((DATA_LEAF_VER as u16) << 8) | 0x05
}
