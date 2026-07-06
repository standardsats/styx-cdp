//! The wallet's config: where the shared deployment artifact lives (`styxnet.toml`), how to
//! reach its own elementsd, where the indexer snapshot persists, and the two wallet keys.
//! Secrets are inline hex - acceptable for the private-network phase, revisit before
//! anything public.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_node::client::Auth;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(String),
    #[error("field {field}: {message}")]
    Field { field: &'static str, message: String },
    #[error("rpc auth: set either rpc_cookie or rpc_user + rpc_password")]
    Auth,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WalletConfig {
    /// The deployment artifact (assets, oracle keys, relays, genesis).
    pub styxnet: PathBuf,
    /// The wallet's own elementsd.
    pub rpc_url: String,
    pub rpc_user: Option<String>,
    pub rpc_password: Option<String>,
    pub rpc_cookie: Option<String>,
    /// The indexer snapshot; created on first sync.
    pub snapshot: PathBuf,
    /// The vault owner key (32-byte hex): what signs owner ops and identifies our vaults.
    pub owner_seckey: String,
    /// The funding key (32-byte hex): the internal key of the wallet's key-path p2tr coins.
    pub funding_seckey: String,
}

pub fn parse_seckey(field: &'static str, hex: &str) -> Result<zkp::Keypair, ConfigError> {
    if hex.len() != 64 {
        return Err(ConfigError::Field { field, message: "expected 32 bytes of hex".into() });
    }
    let mut sk = [0u8; 32];
    for (i, byte) in sk.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| ConfigError::Field { field, message: "bad hex".into() })?;
    }
    zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk)
        .map_err(|e| ConfigError::Field { field, message: e.to_string() })
}

impl WalletConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))
    }

    pub fn auth(&self) -> Result<Auth, ConfigError> {
        match (&self.rpc_cookie, &self.rpc_user, &self.rpc_password) {
            (Some(cookie), None, None) => Ok(Auth::CookieFile(cookie.into())),
            (None, Some(user), Some(pass)) => Ok(Auth::UserPass(user.clone(), pass.clone())),
            _ => Err(ConfigError::Auth),
        }
    }

    pub fn owner(&self) -> Result<zkp::Keypair, ConfigError> {
        parse_seckey("owner_seckey", &self.owner_seckey)
    }

    pub fn funding(&self) -> Result<zkp::Keypair, ConfigError> {
        parse_seckey("funding_seckey", &self.funding_seckey)
    }
}
