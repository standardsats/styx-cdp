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
    /// A self-declared display label the explorer shows instead of the bare slot number
    /// (e.g. the exchange it tracks). Carried on every quote; empty falls back to the slot.
    #[serde(default)]
    pub name: String,
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
    /// The starting price: the effective price until a feed backend delivers (or forever,
    /// without one). POST /price pins a sticky override at runtime; DELETE /price clears it.
    pub price_usd: u32,
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
    /// The live price backend; absent = manual mode (config price + overrides only).
    pub feed: Option<FeedConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeedConfig {
    /// One of: coinbase / binance / kraken / bitstamp / bitfinex. One exchange per slot
    /// keeps the quorum's sources genuinely independent.
    pub backend: String,
    /// Endpoint override (tests and closed networks point a parser at a mock).
    pub url: Option<String>,
    #[serde(default = "default_feed_poll_ms")]
    pub poll_ms: u64,
    /// The staleness gate: stop publishing once the backend has been silent this long
    /// (and until its first delivery). Opt-in - it trades liveness for stale-price
    /// safety: five silent oracles WITH the gate degrade the quorum into a safe freeze,
    /// five without it re-sign a frozen price the covenants cannot tell from a live one.
    pub max_age_secs: Option<u64>,
}

fn default_poll_ms() -> u64 {
    500
}

fn default_feed_poll_ms() -> u64 {
    5_000
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
