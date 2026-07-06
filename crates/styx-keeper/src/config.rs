//! The keeper daemon's config: the purse (same shape as the wallet - the keeper is a
//! wallet that never opens vaults) plus the watchtower knobs.

use std::path::Path;

use serde::Deserialize;
use styx_wallet::config::{ConfigError, WalletConfig};

use crate::keeper::KeeperOpts;

#[derive(Debug, Clone, Deserialize)]
pub struct KeeperConfig {
    #[serde(flatten)]
    pub purse: WalletConfig,
    /// POKE when the issuer anchor lags the tip by more than this many blocks.
    #[serde(default = "default_poke_lag")]
    pub poke_lag: u32,
    /// REFRESH healthy vaults whose ratchet lags by more than this many blocks (M-2).
    #[serde(default = "default_refresh_lag")]
    pub refresh_lag: u32,
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
}

fn default_poke_lag() -> u32 {
    4
}
fn default_refresh_lag() -> u32 {
    16
}
fn default_poll_ms() -> u64 {
    1_000
}

impl KeeperConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))
    }

    pub fn opts(&self) -> KeeperOpts {
        KeeperOpts { poke_lag: self.poke_lag, refresh_lag: self.refresh_lag, ..KeeperOpts::default() }
    }
}
