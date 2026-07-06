//! The explorer's config: the shared deployment artifact, its own node, where the snapshot
//! persists, and the public listener. No keys of any kind - there is nothing here a
//! secret could even configure.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use styx_node::client::Auth;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(String),
    #[error("listen: {0} is not a socket address")]
    Listen(String),
    #[error("rpc auth: set either rpc_cookie or rpc_user + rpc_password")]
    Auth,
    /// The one link the page carries must not smuggle a scheme (javascript:, data:). The
    /// CSP would refuse to run it anyway; this keeps the guarantee CSP-independent.
    #[error("app_url must start with https://, got {0}")]
    AppUrl(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerConfig {
    /// The deployment artifact (assets, oracle keys, relays, genesis).
    pub styxnet: PathBuf,
    /// The explorer's own elementsd.
    pub rpc_url: String,
    pub rpc_user: Option<String>,
    pub rpc_password: Option<String>,
    pub rpc_cookie: Option<String>,
    /// The indexer snapshot; created on first sync.
    pub snapshot: PathBuf,
    /// The HTTP listener. PUBLIC by design (behind the TLS front): this is the hosted
    /// surface, and it holds nothing a listener could leak.
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
    /// The "open a vault" call-to-action target (the styx-app download). Absent, the
    /// section is omitted - and the page then references no external URL at all.
    pub app_url: Option<String>,
}

fn default_listen() -> String {
    "127.0.0.1:9790".into()
}
fn default_poll_ms() -> u64 {
    2_000
}

impl ExplorerConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: ExplorerConfig = toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        cfg.listen_addr()?;
        if let Some(url) = &cfg.app_url {
            if !url.starts_with("https://") {
                return Err(ConfigError::AppUrl(url.clone()));
            }
        }
        Ok(cfg)
    }

    pub fn listen_addr(&self) -> Result<SocketAddr, ConfigError> {
        self.listen.parse().map_err(|_| ConfigError::Listen(self.listen.clone()))
    }

    pub fn auth(&self) -> Result<Auth, ConfigError> {
        match (&self.rpc_cookie, &self.rpc_user, &self.rpc_password) {
            (Some(cookie), None, None) => Ok(Auth::CookieFile(cookie.into())),
            (None, Some(user), Some(pass)) => Ok(Auth::UserPass(user.clone(), pass.clone())),
            _ => Err(ConfigError::Auth),
        }
    }
}
