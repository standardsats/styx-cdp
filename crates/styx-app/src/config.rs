//! The app's config: a wallet purse (the same shape as the wallet CLI - keeper mode runs
//! on the same purse) plus the loopback listener.

use std::net::SocketAddr;
use std::path::Path;

use serde::Deserialize;
use styx_wallet::config::WalletConfig;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(String),
    #[error("listen: {0} is not a socket address")]
    Listen(String),
    /// The app serves a wallet; anything beyond loopback would expose signing to the
    /// network. There is no override: a remote UI is out of scope by design.
    #[error("listen: {0} is not a loopback address (the app only serves 127.0.0.1)")]
    NonLoopback(SocketAddr),
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(flatten)]
    pub purse: WalletConfig,
    /// The HTTP listener; must resolve to a loopback address.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Keeper-mode knobs (the daemon's defaults when absent).
    #[serde(default)]
    pub keeper: KeeperSection,
    /// How often the binary re-assembles a tick from the relays, and the keeper loop's
    /// pace when running.
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeeperSection {
    #[serde(default = "default_poke_lag")]
    pub poke_lag: u32,
    #[serde(default = "default_refresh_lag")]
    pub refresh_lag: u32,
}

impl Default for KeeperSection {
    fn default() -> Self {
        KeeperSection { poke_lag: default_poke_lag(), refresh_lag: default_refresh_lag() }
    }
}

impl KeeperSection {
    pub fn opts(&self) -> styx_keeper::keeper::KeeperOpts {
        styx_keeper::keeper::KeeperOpts {
            poke_lag: self.poke_lag,
            refresh_lag: self.refresh_lag,
            // The app's keeper consumes the shared injected tick; the book walkback is the
            // daemon's concern and stays at its default.
            ..Default::default()
        }
    }
}

fn default_listen() -> String {
    "127.0.0.1:9780".into()
}
fn default_poll_ms() -> u64 {
    2_000
}
fn default_poke_lag() -> u32 {
    4
}
fn default_refresh_lag() -> u32 {
    16
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: AppConfig = toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        cfg.listen_addr()?; // refuse a non-loopback listener before anything runs
        Ok(cfg)
    }

    pub fn listen_addr(&self) -> Result<SocketAddr, ConfigError> {
        let addr: SocketAddr =
            self.listen.parse().map_err(|_| ConfigError::Listen(self.listen.clone()))?;
        if !addr.ip().is_loopback() {
            return Err(ConfigError::NonLoopback(addr));
        }
        Ok(addr)
    }
}
