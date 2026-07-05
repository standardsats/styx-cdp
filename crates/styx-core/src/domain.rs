//! Domain state: what a covenant UTXO commits to, separate from where it lives on chain.

use simplicityhl::elements;
use simplicityhl::elements::secp256k1_zkp::XOnlyPublicKey;

use crate::units::{BlockHeight, Obol, Sats};

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

/// The OBOL pot. Constant address; its balance is the UTXO value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PotState;

/// The stability reserve. Constant address; its balance is the UTXO value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReserveState;

/// The unit a covenant UTXO's value is denominated in: sats for vaults and the reserve,
/// OBOL units for the pot, raw token units (always 1) for the issuer.
pub trait UtxoValue {
    type Value: Copy + std::fmt::Debug + PartialEq + Eq;
}
impl UtxoValue for VaultState {
    type Value = Sats;
}
impl UtxoValue for ReserveState {
    type Value = Sats;
}
impl UtxoValue for PotState {
    type Value = Obol;
}
impl UtxoValue for IssuerState {
    type Value = u64;
}

/// Domain state married to its UTXO. Value and asset are chain facts, not identity facts,
/// so they live here rather than in the state types; the value unit follows the state type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnChain<T: UtxoValue> {
    pub state: T,
    pub outpoint: elements::OutPoint,
    pub value: T::Value,
}

/// The protocol snapshot every builder consumes. One pot, one reserve, one issuer: the
/// single-UTXO invariant (audit L-3) holds by construction - a scanner that finds fragments
/// must refuse to build this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolState {
    pub pot: OnChain<PotState>,
    pub reserve: OnChain<ReserveState>,
    pub issuer: OnChain<IssuerState>,
}
