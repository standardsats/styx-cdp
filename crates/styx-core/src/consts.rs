//! Protocol constants shared by the taproot layer and the covenant params.
//!
//! Everything here is a fixed property of the v1 protocol, not of a particular deploy:
//! the data-leaf version byte, the leaf version-control words the covenants reconstruct
//! Merkle branches with, the canonical NUMS internal key, and the Elements tapleaf tag.

use simplicityhl::elements::hashes::{sha256, Hash};
use simplicityhl::elements::secp256k1_zkp::XOnlyPublicKey;
use std::str::FromStr;

use crate::U256;

/// Taproot leaf version used for the unspendable OP_RETURN data leaves (vault, issuer).
pub const DATA_LEAF_VER: u8 = 0xc4;

/// The canonical BIP341 nothing-up-my-sleeve internal key H (x-only, hex).
/// Every covenant output is finalized to this key, so no key-path spend exists.
pub const INTERNAL_KEY_HEX: &str = "50929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0";

/// The canonical NUMS internal key as an x-only pubkey.
pub fn nums_key() -> XOnlyPublicKey {
    // A constant that parses by construction; `preflight` re-checks canonicality at deploy time.
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
