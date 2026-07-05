//! Shared fixtures for the integration-test binaries. Each test binary compiles its own copy;
//! the artifacts cache is per binary.

#![allow(dead_code)]

use simplicityhl::elements::secp256k1_zkp as zkp;
use simplicityhl::elements::AssetId;

use styx_core::artifacts::Artifacts;
use styx_core::oracle::{OracleSlot, OracleTick, SignedQuote};
use styx_core::params::Params;
use styx_core::units::{BlockHeight, Price, RatioK};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn keypair(secret: u8) -> zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    zkp::Keypair::from_seckey_slice(&zkp::Secp256k1::new(), &sk).unwrap()
}

pub fn xonly(secret: u8) -> zkp::XOnlyPublicKey {
    keypair(secret).x_only_public_key().0
}

pub fn asset(byte: u8) -> AssetId {
    AssetId::from_slice(&[byte; 32]).unwrap()
}

/// The fixed test deploy: dummy assets, the prototype's five oracle secrets.
pub fn test_params() -> Params {
    Params {
        obol: asset(0x01),
        issuer_token: asset(0x02),
        policy: asset(0x03),
        oracle_pks: [xonly(7), xonly(8), xonly(9), xonly(101), xonly(102)],
    }
}

pub fn test_artifacts() -> &'static Artifacts {
    static ARTIFACTS: std::sync::OnceLock<Artifacts> = std::sync::OnceLock::new();
    ARTIFACTS.get_or_init(|| Artifacts::compile(&test_params()).unwrap())
}

/// A tick with dummy signatures in slots 0..2, for tests that never verify them.
pub fn dummy_tick() -> OracleTick {
    let quote = SignedQuote { price: Price::new(120_000), sig: [0u8; 64] };
    OracleTick::new(
        BlockHeight::new(100),
        RatioK::from_cr_percent(100),
        [
            (OracleSlot::new(0).unwrap(), quote),
            (OracleSlot::new(1).unwrap(), quote),
            (OracleSlot::new(2).unwrap(), quote),
        ],
    )
    .unwrap()
}
