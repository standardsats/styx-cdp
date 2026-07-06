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
    /// The platform-proxy relaxation, absent by default. Present ONLY for Umbrel / StartOS,
    /// where the app runs in one container and the platform's authenticated proxy runs in
    /// another: the app binds where the proxy can reach it (not loopback) and answers to
    /// the proxy's hostnames. This is the single sanctioned way past the loopback rule, and
    /// it is opt-in config, not a silent manifest default.
    pub proxy: Option<ProxyConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    /// Where to actually listen - reachable from the proxy container (e.g. `0.0.0.0:9780`).
    /// The loopback rule does not apply here BY DESIGN; the platform's tunnel and auth are
    /// the perimeter instead.
    pub bind: String,
    /// The exact Host-header values the platform proxy forwards, port included where it
    /// sends one (`umbrel.local:9780`, `<onion>.onion`, `styx.local`).
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// The exact Origin values the proxied browser sends, scheme included and matched whole.
    /// Configured, not inferred: Umbrel LAN and Tor onion are plain `http://`, only a
    /// StartOS LAN cert is `https://` (`["http://umbrel.local:9780", "http://<onion>.onion"]`).
    #[serde(default)]
    pub allow_origins: Vec<String>,
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
        cfg.bind()?; // resolve the bind now: a bad or (non-proxy) non-loopback bind fails early
        Ok(cfg)
    }

    /// How the app binds and which hosts/origins it answers to: `(addr, proxy_hosts,
    /// proxy_origins)`. `[proxy]` present is the explicit platform relaxation (bind
    /// anywhere, add its hostnames and origins); otherwise the loopback rule holds, with
    /// `STYX_APP_LISTEN` a loopback-checked override for the desktop shell.
    pub fn bind(&self) -> Result<(SocketAddr, Vec<String>, Vec<String>), ConfigError> {
        if let Some(proxy) = &self.proxy {
            let addr: SocketAddr =
                proxy.bind.parse().map_err(|_| ConfigError::Listen(proxy.bind.clone()))?;
            return Ok((addr, proxy.allow_hosts.clone(), proxy.allow_origins.clone()));
        }
        let addr = self.resolve_listen(std::env::var("STYX_APP_LISTEN").ok().as_deref())?;
        Ok((addr, Vec::new(), Vec::new()))
    }

    /// The bind address for the loopback path: `STYX_APP_LISTEN` overrides the config, but
    /// the loopback rule holds for both - an env var cannot open the app to the network any
    /// more than the config file can.
    pub fn resolve_listen(&self, env: Option<&str>) -> Result<SocketAddr, ConfigError> {
        let raw = env.unwrap_or(&self.listen);
        let addr: SocketAddr = raw.parse().map_err(|_| ConfigError::Listen(raw.to_string()))?;
        if !addr.ip().is_loopback() {
            return Err(ConfigError::NonLoopback(addr));
        }
        Ok(addr)
    }
}
