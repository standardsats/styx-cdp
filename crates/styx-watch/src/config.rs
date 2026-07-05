//! `styxnet.toml`: the deployment artifact every machine shares. The operator writes the
//! skeleton (oracles, relays); `styx-deploy run` executes the ceremony and fills the
//! chain-derived sections (assets, genesis, the issuer's genesis anchor).

use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::elements::{AssetId, BlockHash};
use styx_core::params::Params;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(String),
    #[error("field {field}: {message}")]
    Field { field: &'static str, message: String },
    #[error("the ceremony has not run yet: [{0}] is missing")]
    NotDeployed(&'static str),
    #[error("expected exactly 5 oracles at distinct slots 0..=4, got {0}")]
    OracleSet(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyxnetConfig {
    pub network: Network,
    /// Filled by `styx-deploy run`.
    pub assets: Option<Assets>,
    /// Filled by `styx-deploy run`.
    pub protocol: Option<Protocol>,
    #[serde(default)]
    pub oracles: Vec<OracleEntry>,
    pub nostr: Nostr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    /// The chain name (`-chain=`), documentation more than mechanism.
    pub chain: String,
    /// The genesis block hash; every consumer checks it against its own node before acting.
    pub genesis: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assets {
    pub obol: String,
    pub issuer_token: String,
    pub policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Protocol {
    /// The issuer anchor at deployment (the ceremony's height): the scan floor for
    /// `find_issuer` recovery.
    pub issuer_anchor_genesis: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleEntry {
    pub slot: u8,
    /// The protocol key the covenants verify quotes against (x-only hex).
    pub protocol_pk: String,
    /// The Nostr transport identity (filled before R2's quote client goes live).
    pub nostr_pk: Option<String>,
    /// The debug/admin endpoint (scenario control).
    pub admin_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nostr {
    #[serde(default)]
    pub relays: Vec<String>,
}

impl StyxnetConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| ConfigError::Toml(e.to_string()))
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Toml(e.to_string()))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// The five oracle protocol keys in slot order.
    pub fn oracle_pks(&self) -> Result<[XOnlyPublicKey; 5], ConfigError> {
        let mut pks: [Option<XOnlyPublicKey>; 5] = [None; 5];
        for o in &self.oracles {
            let i = o.slot as usize;
            if i > 4 || pks[i].is_some() {
                return Err(ConfigError::OracleSet(format!("bad or duplicate slot {}", o.slot)));
            }
            pks[i] = Some(XOnlyPublicKey::from_str(&o.protocol_pk).map_err(|e| ConfigError::Field {
                field: "oracles.protocol_pk",
                message: e.to_string(),
            })?);
        }
        let mut out = Vec::with_capacity(5);
        for (i, pk) in pks.into_iter().enumerate() {
            out.push(pk.ok_or_else(|| ConfigError::OracleSet(format!("slot {i} missing")))?);
        }
        out.try_into().map_err(|_| ConfigError::OracleSet("shape".into()))
    }

    /// The covenant params, once the ceremony sections are present.
    pub fn params(&self) -> Result<Params, ConfigError> {
        let assets = self.assets.as_ref().ok_or(ConfigError::NotDeployed("assets"))?;
        let parse = |field: &'static str, s: &str| {
            AssetId::from_str(s).map_err(|e| ConfigError::Field { field, message: e.to_string() })
        };
        Ok(Params {
            obol: parse("assets.obol", &assets.obol)?,
            issuer_token: parse("assets.issuer_token", &assets.issuer_token)?,
            policy: parse("assets.policy", &assets.policy)?,
            oracle_pks: self.oracle_pks()?,
        })
    }

    pub fn genesis(&self) -> Result<BlockHash, ConfigError> {
        let g = self.network.genesis.as_ref().ok_or(ConfigError::NotDeployed("network.genesis"))?;
        BlockHash::from_str(g)
            .map_err(|e| ConfigError::Field { field: "network.genesis", message: e.to_string() })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SKELETON: &str = r#"
[network]
chain = "styxnet"

[[oracles]]
slot = 0
protocol_pk = "50929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0"

[nostr]
relays = ["ws://10.0.0.5:8080"]
"#;

    #[test]
    fn skeleton_parses_and_reports_not_deployed() {
        let cfg: StyxnetConfig = toml::from_str(SKELETON).unwrap();
        assert_eq!(cfg.network.chain, "styxnet");
        assert!(matches!(cfg.params(), Err(ConfigError::NotDeployed("assets"))));
        assert!(matches!(cfg.genesis(), Err(ConfigError::NotDeployed("network.genesis"))));
    }

    #[test]
    fn oracle_set_must_be_complete_and_distinct() {
        let cfg: StyxnetConfig = toml::from_str(SKELETON).unwrap();
        assert!(matches!(cfg.oracle_pks(), Err(ConfigError::OracleSet(_))));
        let mut full = cfg.clone();
        for slot in 1..5u8 {
            full.oracles.push(OracleEntry {
                slot,
                protocol_pk: full.oracles[0].protocol_pk.clone(),
                nostr_pk: None,
                admin_url: None,
            });
        }
        // Five distinct slots parse (key duplication is preflight's job, not the config's).
        assert!(full.oracle_pks().is_ok());
        full.oracles[4].slot = 3;
        assert!(matches!(full.oracle_pks(), Err(ConfigError::OracleSet(_))));
    }
}
