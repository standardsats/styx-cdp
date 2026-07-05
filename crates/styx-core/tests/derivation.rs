//! Derivation goldens: data-leaf bytes, leaf version words, NUMS, tapleaf tag, and the
//! taproot scriptPubKeys under a fixed test deploy.
//!
//! The spk goldens pin the whole derivation chain (covenant CMR -> leaf -> tree -> tweak):
//! a change in tree shape, leaf order, or leaf version flips them even when the CMRs alone
//! would not.

mod common;

use common::{hex, test_artifacts, test_params, xonly};
use simplicityhl::elements::hashes::{sha256, Hash};
use simplicityhl::elements::secp256k1_zkp as zkp;

use styx_core::consts::{issuer_leaf_vc, leaf_vc, nums_key, tapleaf_tag, INTERNAL_KEY_HEX};
use styx_core::domain::{IssuerState, VaultState};
use styx_core::leaves::{issuer_data_leaf, vault_data_leaf};
use styx_core::params::PreflightError;
use styx_core::units::{BlockHeight, Obol};

// --- leaf encodings ---------------------------------------------------------

#[test]
fn vault_data_leaf_bytes_golden() {
    // OP_RETURN || debt(8 BE) || owner(32) || last_height(4 BE) = 45 bytes.
    let owner = nums_key();
    let state =
        VaultState { debt: Obol::new(0x0102030405060708), owner, last_height: BlockHeight::new(1234) };
    let (script, _ver) = vault_data_leaf(&state);
    assert_eq!(hex(script.as_bytes()), format!("6a0102030405060708{INTERNAL_KEY_HEX}000004d2"));
    assert_eq!(script.len(), 45);
}

#[test]
fn issuer_data_leaf_bytes_golden() {
    // OP_RETURN || last_mint_height(4 BE) = 5 bytes.
    let (script, _ver) = issuer_data_leaf(BlockHeight::new(0xa1b2c3d4));
    assert_eq!(hex(script.as_bytes()), "6aa1b2c3d4");
}

#[test]
fn leaf_version_words() {
    // version byte 0xc4 || compact_size(leaf length): 45 for the vault, 5 for the issuer.
    assert_eq!(leaf_vc(), 0xc42d);
    assert_eq!(issuer_leaf_vc(), 0xc405);
}

#[test]
fn nums_is_the_canonical_bip341_h() {
    // BIP341 defines H constructively: the x coordinate is sha256 of the uncompressed
    // encoding of the generator G. Deriving it here (rather than re-parsing the constant)
    // is what proves the discrete log of H is unknown; a NUMS with a known dlog would let
    // its holder key-path spend every covenant UTXO.
    let secp = zkp::Secp256k1::new();
    let one = zkp::SecretKey::from_slice(&{
        let mut b = [0u8; 32];
        b[31] = 1;
        b
    })
    .unwrap();
    let g = zkp::PublicKey::from_secret_key(&secp, &one); // 1 * G == G
    let h = sha256::Hash::hash(&g.serialize_uncompressed()).to_byte_array();
    assert_eq!(hex(&h), INTERNAL_KEY_HEX);
    assert_eq!(hex(&nums_key().serialize()), INTERNAL_KEY_HEX);
}

#[test]
fn tapleaf_tag_is_sha256_of_tapleaf_elements() {
    let expected = sha256::Hash::hash(b"TapLeaf/elements").to_byte_array();
    assert_eq!(tapleaf_tag().to_byte_array(), expected);
}

// --- taproot derivation under the test deploy --------------------------------

// Recorded from the first green run under `test_params()`; any change to covenant sources,
// tree shape, leaf order, or leaf versions flips them.
const POT_SPK_HASH: &str = "74c3c6db4a2b854125951d18fa78c2b647ce5234c8e32d41b1d765f1f0cea936";
const STABILITY_SPK_HASH: &str = "0003002c42cdc73af7421ab0e6ca9a2bad7b5d6544bdbc21079e8c8d3c35d836";
const VAULT_CMR: &str = "5b3a8ba32a1be8efc469c30ddbe58f5489b7139f2288d412f9e660ccb7394b43";
const VAULT_SPK: &str = "512049d5c980f8170b4a7b2929f53b45d3cb9b622be691b6e9b494fe37751e8ad952";
const ISSUER_SPK: &str = "512023195c3907f7f0b3320f3e0d3432fcae6194620bbf4f52172953524f359ac497";

#[test]
fn pot_spk_hash_golden() {
    assert_eq!(hex(&test_artifacts().pot_spk_hash().to_byte_array()), POT_SPK_HASH);
}

#[test]
fn stability_spk_hash_golden() {
    assert_eq!(hex(&test_artifacts().stability_spk_hash().to_byte_array()), STABILITY_SPK_HASH);
}

#[test]
fn vault_cmr_golden() {
    assert_eq!(hex(&test_artifacts().vault_cmr().to_byte_array()), VAULT_CMR);
}

#[test]
fn vault_spk_golden() {
    let state =
        VaultState { debt: Obol::new(5_000_000), owner: nums_key(), last_height: BlockHeight::new(100) };
    assert_eq!(hex(test_artifacts().vault_spk(&state).as_bytes()), VAULT_SPK);
}

#[test]
fn issuer_spk_golden() {
    let state = IssuerState { last_mint_height: BlockHeight::new(100) };
    assert_eq!(hex(test_artifacts().issuer_spk(&state).as_bytes()), ISSUER_SPK);
}

#[test]
fn vault_spk_commits_to_every_state_field() {
    let a = test_artifacts();
    let base =
        VaultState { debt: Obol::new(5_000_000), owner: nums_key(), last_height: BlockHeight::new(100) };
    let spk = a.vault_spk(&base);
    assert_ne!(spk, a.vault_spk(&VaultState { debt: Obol::new(5_000_001), ..base }));
    assert_ne!(spk, a.vault_spk(&VaultState { last_height: BlockHeight::new(101), ..base }));
    assert_ne!(spk, a.vault_spk(&VaultState { owner: xonly(7), ..base }));
}

#[test]
fn issuer_spk_varies_with_anchor() {
    let a = test_artifacts();
    assert_ne!(
        a.issuer_spk(&IssuerState { last_mint_height: BlockHeight::new(100) }),
        a.issuer_spk(&IssuerState { last_mint_height: BlockHeight::new(101) })
    );
}

// --- deploy preflight ---------------------------------------------------------

#[test]
fn preflight_accepts_the_test_deploy() {
    assert!(test_params().preflight().is_ok());
}

#[test]
fn preflight_rejects_duplicate_oracle_keys() {
    let mut p = test_params();
    p.oracle_pks[3] = p.oracle_pks[0];
    assert_eq!(p.preflight(), Err(PreflightError::DuplicateOracleKey { a: 0, b: 3 }));
}

#[test]
fn preflight_rejects_negated_oracle_key() {
    // x-only keys drop the y parity, so the secrets s and n-s produce the same public key:
    // two "different" signers that alias to one oracle slot key.
    let secp = zkp::Secp256k1::new();
    let sk = zkp::SecretKey::from_slice(&{
        let mut b = [0u8; 32];
        b[31] = 7;
        b
    })
    .unwrap();
    let negated = zkp::Keypair::from_secret_key(&secp, &sk.negate());
    assert_eq!(negated.x_only_public_key().0, xonly(7)); // the alias itself
    let mut p = test_params();
    p.oracle_pks[1] = negated.x_only_public_key().0;
    assert_eq!(p.preflight(), Err(PreflightError::DuplicateOracleKey { a: 0, b: 1 }));
}

#[test]
fn preflight_rejects_asset_collisions() {
    let mut p = test_params();
    p.issuer_token = p.obol;
    assert!(matches!(p.preflight(), Err(PreflightError::AssetCollision { .. })));

    let mut p = test_params();
    p.policy = p.obol;
    assert!(matches!(p.preflight(), Err(PreflightError::AssetCollision { .. })));
}
