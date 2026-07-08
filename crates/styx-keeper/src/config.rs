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
    /// POKE when the issuer anchor lags the tip by more than this many blocks. The cost/risk
    /// knob for the idle keeper: a poke advances the anchor, so this bounds how stale a tick a
    /// mint may reuse during quiet spells. Lower = fresher mint-price floor but a poke (one
    /// fee) roughly every `poke_lag + 1` blocks; higher = fewer pokes and a cheaper idle
    /// keeper, at a looser staleness bound. Mints and attests refresh the anchor for free, so
    /// on an active system pokes are rare regardless.
    #[serde(default = "default_poke_lag")]
    pub poke_lag: u32,
    /// REFRESH a healthy vault whose ratchet lags the tip by more than this many blocks (M-2).
    /// Same cost/risk shape: lower keeps vaults actionable sooner (a vault acts only on a tick
    /// above its ratchet) but spends more on refresh fees; higher is cheaper but leaves a vault
    /// briefly unactionable after a long quiet spell.
    #[serde(default = "default_refresh_lag")]
    pub refresh_lag: u32,
    /// How far below the tip the tick assembly walks looking for a quote quorum.
    #[serde(default = "default_walkback")]
    pub walkback: u32,
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
}

fn default_poke_lag() -> u32 {
    4
}
fn default_refresh_lag() -> u32 {
    16
}
fn default_walkback() -> u32 {
    8
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
        KeeperOpts { poke_lag: self.poke_lag, refresh_lag: self.refresh_lag, walkback: self.walkback }
    }
}
