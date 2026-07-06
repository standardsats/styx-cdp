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
}

fn default_listen() -> String {
    "127.0.0.1:9780".into()
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
