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
    /// The Host values this listener answers to directly: the bound address as written
    /// (`127.0.0.1:p`, `[::1]:p`) plus `localhost:p`. The loopback page is reached over
    /// plain http, so its Origins are `http://<one of these>`.
    hosts: Vec<String>,
    /// Extra Host values reached through a platform's proxy (Umbrel / StartOS): the
    /// EXPLICIT, opt-in relaxation of the loopback rule. Exact Host-header form, PORT
    /// INCLUDED where the platform sends one (`umbrel.local:9780`). Empty without a
    /// `[proxy]` section.
    proxy_hosts: Vec<String>,
    /// The full Origin strings the proxied browser sends, matched whole - scheme included.
    /// The platforms are NOT uniformly https: Umbrel serves apps over plain http on the LAN
    /// and Tor onion origins are `http://` too (Tor is the transport security); only a
    /// StartOS LAN cert is https. So the scheme is configured, not inferred - requiring
    /// https would 403 two of the three real paths.
    proxy_origins: Vec<String>,
}

impl Gate {
    /// Mint a loopback session gate: a fresh 32-byte token, hex-encoded. Minting from the
    /// address (not just the port) keeps the allowlist in lockstep with the config's
    /// loopback choice - a `[::1]` listener must answer to `Host: [::1]:p`.
    pub fn mint(addr: std::net::SocketAddr) -> Gate {
        Gate::mint_proxied(addr, Vec::new(), Vec::new())
    }

    /// Mint a gate that ALSO answers to a platform proxy: `proxy_hosts` on the Host header
    /// and `proxy_origins` on the Origin header (both exact). The token gate still applies
    /// to every `/api` call; the platform's authenticated tunnel is the perimeter in front.
    pub fn mint_proxied(
        addr: std::net::SocketAddr,
        proxy_hosts: Vec<String>,
        proxy_origins: Vec<String>,
    ) -> Gate {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect();
        // `localhost:port` and the bound-address form are auto-trusted ONLY for a loopback
        // bind. On a non-loopback (proxy) bind the port is on a wider network, and a
        // `Host: localhost:port` from an attacker would otherwise pass and be handed the
        // token - so a proxy deploy trusts nothing but its explicit allow_hosts.
        let hosts = if addr.ip().is_loopback() {
            vec![addr.to_string(), format!("localhost:{}", addr.port())]
        } else {
            Vec::new()
        };
        Gate { token, hosts, proxy_hosts, proxy_origins }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// The Host header must name this listener or a configured proxy host, exactly.
    fn host_ok(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h == host) || self.proxy_hosts.iter().any(|h| h == host)
    }

    /// A loopback Origin is `http://<a bound host>`; a proxy Origin must match a configured
    /// full origin verbatim. Neither a same-machine `https://127.0.0.1` (no TLS there) nor a
    /// cross-site origin is either.
    fn origin_ok(&self, origin: &str) -> bool {
        if let Some(rest) = origin.strip_prefix("http://") {
            if self.hosts.iter().any(|h| h == rest) {
                return true;
            }
        }
        self.proxy_origins.iter().any(|o| o == origin)
    }

    /// The page-tier check: Host (anti-rebinding), Origin, and `Sec-Fetch-Site` - no token.
    /// The served page is where the browser LEARNS the token, so it cannot require one; a
    /// foreign process on the same machine is outside this gate's threat model either way
    /// (it can read the config file, which holds the keys themselves).
    ///
    /// The `Sec-Fetch-Site` check is what stops a hostile page from stealing the token with
    /// a cross-origin `<script src=".../session.js">`: that request carries no Origin (so
    /// the Origin check alone waves it through), but the browser stamps it
    /// `Sec-Fetch-Site: cross-site`. We reject `cross-site` / `same-site` and accept only
    /// `same-origin`, `none` (a typed URL / direct navigation), or its absence (curl and
    /// other non-browsers, which the token still gates on `/api`).
    pub fn check_page(&self, headers: &HeaderMap) -> Result<(), GateError> {
        if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
            if site == "cross-site" || site == "same-site" {
                return Err(GateError::ForbiddenOrigin);
            }
        }
        let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
        if !self.host_ok(host) {
            return Err(GateError::ForbiddenHost);
        }
        if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
            if !self.origin_ok(origin) {
                return Err(GateError::ForbiddenOrigin);
            }
        }
        Ok(())
    }

    pub fn check(&self, headers: &HeaderMap) -> Result<(), GateError> {
        self.check_page(headers)?;
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

/// The page-tier middleware: `Gate::check_page`.
pub async fn page_gate(State(g): State<Arc<Gate>>, req: Request, next: Next) -> Response {
    match g.check_page(req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => (e.status(), format!("{e:?}")).into_response(),
    }
}
