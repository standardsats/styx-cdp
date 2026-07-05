//! The immutable per-deploy configuration.
//!
//! These values are baked into the covenant params at compile time; the covenants cannot
//! re-check them at spend time, so a wrong value is unfixable after deploy. `preflight`
//! checks everything checkable from the config alone. A real deployment must additionally
//! compare the compiled artifacts against the ones it actually publishes on chain - that
//! comparison needs the published side and belongs to the deploy tooling (M11).

use simplicityhl::elements::encode::serialize;
use simplicityhl::elements::secp256k1_zkp::XOnlyPublicKey;
use simplicityhl::elements::AssetId;

use crate::U256;

#[derive(Debug, Clone)]
pub struct Params {
    /// The OBOL asset: the stable asset minted against vault collateral.
    pub obol: AssetId,
    /// The issuer's 1-unit identity token. Distinct from OBOL and non-reissuable.
    pub issuer_token: AssetId,
    /// The chain's policy asset (L-BTC): collateral and the reserve's balance.
    pub policy: AssetId,
    /// The five oracle keys of the 3-of-5 quorum, pairwise distinct.
    pub oracle_pks: [XOnlyPublicKey; 5],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    /// Two oracle slots hold the same x-only key. The quorum's <=2-Byzantine model rests on
    /// five distinct entities; with an aliased slot, count == 3 is reachable with two real
    /// signers. Keys are compared rather than secrets: x-only drops the y parity, so the
    /// secrets s and n-s produce the same key.
    #[error("ORACLE_PK_{} == ORACLE_PK_{}: the 3-of-5 fault model is broken", .a + 1, .b + 1)]
    DuplicateOracleKey { a: usize, b: usize },
    /// Two of OBOL / issuer token / policy are the same asset.
    #[error("asset collision: {a} == {b}")]
    AssetCollision { a: &'static str, b: &'static str },
}

impl Params {
    /// Deploy-time checks, run before any UTXO exists.
    pub fn preflight(&self) -> Result<(), PreflightError> {
        for a in 0..5 {
            for b in (a + 1)..5 {
                if self.oracle_pks[a] == self.oracle_pks[b] {
                    return Err(PreflightError::DuplicateOracleKey { a, b });
                }
            }
        }
        let assets = [
            ("OBOL_ID", self.obol),
            ("ISSUER_TOKEN_ID", self.issuer_token),
            ("POLICY", self.policy),
        ];
        for i in 0..assets.len() {
            for j in (i + 1)..assets.len() {
                if assets[i].1 == assets[j].1 {
                    return Err(PreflightError::AssetCollision { a: assets[i].0, b: assets[j].0 });
                }
            }
        }
        Ok(())
    }
}

/// An asset id as the u256 covenant param, in consensus-serialization byte order.
pub(crate) fn asset_u256(id: AssetId) -> U256 {
    let bytes = serialize(&id);
    #[allow(clippy::unwrap_used)] // an AssetId serializes to exactly 32 bytes
    U256::from_byte_array(bytes.try_into().unwrap())
}

/// An x-only key as the u256 covenant param.
pub(crate) fn xonly_u256(pk: &XOnlyPublicKey) -> U256 {
    U256::from_byte_array(pk.serialize())
}
