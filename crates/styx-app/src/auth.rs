//! The request gate: a per-session token plus a Host/Origin allowlist.
//!
//! The token (minted at startup, printed once) makes every call unforgeable by other local
//! processes and by scripts in a browser. The Host check refuses DNS-rebinding (a foreign
//! name resolving to 127.0.0.1 still carries the foreign name in Host); the Origin check
//! refuses cross-origin browser calls outright. An absent Origin is allowed - curl and
//! same-origin GETs do not send one; the token still gates them.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rand::RngCore;
use std::sync::Arc;

pub const TOKEN_HEADER: &str = "x-styx-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// Missing or wrong token.
    Unauthorized,
    /// A Host that is not this listener: the DNS-rebinding shape.
    ForbiddenHost,
    /// A cross-origin browser call.
    ForbiddenOrigin,
}

impl GateError {
    fn status(self) -> StatusCode {
        match self {
            GateError::Unauthorized => StatusCode::UNAUTHORIZED,
            GateError::ForbiddenHost | GateError::ForbiddenOrigin => StatusCode::FORBIDDEN,
        }
    }
}

pub struct Gate {
    token: String,
    /// The exact Host values this listener answers to: the bound address as written
    /// (`127.0.0.1:p`, `[::1]:p`, whatever loopback the config chose) plus `localhost:p`.
    hosts: [String; 2],
}

impl Gate {
    /// Mint a session gate for the actual bound address: a fresh 32-byte token,
    /// hex-encoded. Minting from the address (not just the port) keeps the allowlist in
    /// lockstep with the config's loopback choice - a `[::1]` listener must answer to
    /// `Host: [::1]:p`.
    pub fn mint(addr: std::net::SocketAddr) -> Gate {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Gate { token, hosts: [addr.to_string(), format!("localhost:{}", addr.port())] }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn host_ok(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h == host)
    }

    pub fn check(&self, headers: &HeaderMap) -> Result<(), GateError> {
        let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
        if !self.host_ok(host) {
            return Err(GateError::ForbiddenHost);
        }
        if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
            let ok = origin.strip_prefix("http://").map(|rest| self.host_ok(rest)).unwrap_or(false);
            if !ok {
                return Err(GateError::ForbiddenOrigin);
            }
        }
        match headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) {
            Some(t) if token_eq(t, &self.token) => Ok(()),
            _ => Err(GateError::Unauthorized),
        }
    }
}

/// Timing-resistant token comparison: compare the SHA256 digests instead of the strings
/// (the bitcoind RPC-auth pattern). A byte-wise early exit then leaks digest prefixes,
/// which say nothing about the token itself.
fn token_eq(a: &str, b: &str) -> bool {
    use styx_core::elements::hashes::{sha256, Hash};
    sha256::Hash::hash(a.as_bytes()) == sha256::Hash::hash(b.as_bytes())
}

/// The axum middleware over `Gate::check`.
pub async fn gate(State(g): State<Arc<Gate>>, req: Request, next: Next) -> Response {
    match g.check(req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => (e.status(), format!("{e:?}")).into_response(),
    }
}
