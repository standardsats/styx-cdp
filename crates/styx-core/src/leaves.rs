//! The unspendable OP_RETURN data leaves committed in the vault and issuer taproot trees.
//!
//! Each leaf's first byte is OP_RETURN, so it can never be executed as a script path; it
//! exists to commit state into the address. The covenants reconstruct these leaves byte for
//! byte when they verify a successor output, so the encodings here are consensus-critical:
//! vault = OP_RETURN || debt(8 BE) || owner(32) || last_height(4 BE), 45 bytes;
//! issuer = OP_RETURN || last_mint_height(4 BE), 5 bytes. The matching version-control words
//! (`leaf_vc`, `issuer_leaf_vc`) live in `consts`.

use simplicityhl::elements::taproot::LeafVersion;
use simplicityhl::elements::Script;

use crate::consts::DATA_LEAF_VER;
use crate::domain::VaultState;
use crate::units::BlockHeight;

/// The taproot leaf version of every data leaf.
pub fn data_leaf_version() -> LeafVersion {
    #[allow(clippy::unwrap_used)] // 0xc4 is a valid leaf version byte
    LeafVersion::from_u8(DATA_LEAF_VER).unwrap()
}

/// The vault's 45-byte data leaf.
pub fn vault_data_leaf(state: &VaultState) -> (Script, LeafVersion) {
    let mut s = vec![0x6au8];
    s.extend_from_slice(&state.debt.raw().to_be_bytes());
    s.extend_from_slice(&state.owner.serialize());
    s.extend_from_slice(&state.last_height.raw().to_be_bytes());
    (Script::from(s), data_leaf_version())
}

/// The issuer's 5-byte data leaf.
pub fn issuer_data_leaf(last_mint_height: BlockHeight) -> (Script, LeafVersion) {
    let mut s = vec![0x6au8];
    s.extend_from_slice(&last_mint_height.raw().to_be_bytes());
    (Script::from(s), data_leaf_version())
}
