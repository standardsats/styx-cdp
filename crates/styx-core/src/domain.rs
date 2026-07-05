//! Domain state: what a covenant UTXO commits to, separate from where it lives on chain.

use simplicityhl::elements::secp256k1_zkp::XOnlyPublicKey;

use crate::units::{BlockHeight, Obol};

/// A vault's identity: the fields committed in its data leaf. The taproot output derives from
/// these (`Artifacts::vault_spk`), so a state and its address cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultState {
    pub debt: Obol,
    /// Stricter than the covenant: OPEN commits any u256 as the owner, so a vault whose owner
    /// bytes are not a valid x-only point can exist on chain (unspendable by owner ops, still
    /// liquidatable). Builders only ever create key-owned vaults; the state scanner must skip
    /// such foreign UTXOs rather than fail.
    pub owner: XOnlyPublicKey,
    /// The freshness ratchet: the most recent oracle height this vault acknowledged.
    pub last_height: BlockHeight,
}

/// The issuer singleton's state: the global mint-recency anchor committed in its data leaf,
/// advanced on every open, draw, poke, and bad-debt attest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssuerState {
    pub last_mint_height: BlockHeight,
}
