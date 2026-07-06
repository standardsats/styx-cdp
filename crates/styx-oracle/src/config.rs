//! The oracle daemon's config: one file per daemon (slot, keys, relay set, its own
//! elementsd, the admin listener, the starting price). Secrets are inline hex - acceptable
//! for the private-network phase, revisit before anything public.

use std::path::Path;

use serde::Deserialize;
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
    #[error("price_usd must be positive: an oracle never signs a zero price")]
    ZeroPrice,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OracleConfig {
    /// The oracle's quorum slot (0..=4), matching its key's position in styxnet.toml.
    pub slot: u8,
    /// The covenant oracle secret key (32-byte hex) - what signs tick digests.
    pub protocol_seckey: String,
    /// The Nostr transport secret key (hex or nsec) - carriage identity only.
    pub nostr_seckey: String,
    pub relays: Vec<String>,
    /// The daemon's own elementsd.
    pub rpc_url: String,
    pub rpc_user: Option<String>,
    pub rpc_password: Option<String>,
    pub rpc_cookie: Option<String>,
    /// The debug/admin HTTP listener. Unauthenticated by design in this phase - it controls
    /// the signed price and signs quotes on demand, so bind loopback (or a trusted LAN at
    /// most), never a public interface.
    pub listen: String,
    /// The starting price; POST /price moves it at runtime (scenario control).
    pub price_usd: u32,
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
}

fn default_poll_ms() -> u64 {
    500
}

impl OracleConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        // The twin of the POST /price refusal: the daemon must not come up signing zeroes.
        if cfg.price_usd == 0 {
            return Err(ConfigError::ZeroPrice);
        }
        Ok(cfg)
    }

    pub fn auth(&self) -> Result<Auth, ConfigError> {
        match (&self.rpc_cookie, &self.rpc_user, &self.rpc_password) {
            (Some(cookie), None, None) => Ok(Auth::CookieFile(cookie.into())),
            (None, Some(user), Some(pass)) => Ok(Auth::UserPass(user.clone(), pass.clone())),
            _ => Err(ConfigError::Auth),
        }
    }
}
