//! The served UI: static assets embedded in the binary, one template substitution (the
//! session token into the index page - the page is where the browser learns it, behind
//! the page-tier Host/Origin gate; a same-machine process is outside this threat model,
//! it can read the config's keys directly).
//!
//! The CSP pins everything to this origin: no external script, style, font, or connection
//! can even be requested - "no CDN, no telemetry" as a browser-enforced property on top
//! of the no-external-URL asset test.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::auth::{page_gate, Gate};

const INDEX: &str = include_str!("../assets/index.html");
const CSS: &str = include_str!("../assets/app.css");
const JS: &str = include_str!("../assets/app.js");
// The explorer's meander mark with the middle bar in river blue, so an app tab and an
// explorer tab read apart at a glance. Self-hosted: img-src stays 'self'.
const FAVICON: &str = include_str!("../assets/favicon.svg");
// The spec-site's faces, one source of truth (the heavy body serif is a fallback stack).
const DIDOT_LATIN: &[u8] = include_bytes!("../../../spec-site/fonts/gfs-didot-normal-400-latin.woff2");
const DIDOT_GREEK: &[u8] = include_bytes!("../../../spec-site/fonts/gfs-didot-normal-400-greek.woff2");
const PLEX_400: &[u8] = include_bytes!("../../../spec-site/fonts/ibm-plex-mono-normal-400-latin.woff2");
const PLEX_500: &[u8] = include_bytes!("../../../spec-site/fonts/ibm-plex-mono-normal-500-latin.woff2");

pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; \
     connect-src 'self'; img-src 'self'; form-action 'self'; base-uri 'none'; \
     frame-ancestors 'none'";

/// Every asset the binary serves, for the no-external-URL sweep in the fast tier.
pub fn all_assets() -> [(&'static str, &'static str); 4] {
    [("index.html", INDEX), ("app.css", CSS), ("app.js", JS), ("favicon.svg", FAVICON)]
}

/// The hardening headers every page-tier response carries. `frame-ancestors` has no
/// default-src fallback and X-Frame-Options covers older engines: an invisible-iframe
/// clickjack of the wallet is a griefing surface (close, keeper stop) even though every
/// op only ever pays the wallet's own scripts.
fn harden() -> [(header::HeaderName, &'static str); 3] {
    [
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ]
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")],
        harden(),
        INDEX,
    )
}

/// The token as a same-origin script: keeps the page free of inline JS, so `script-src
/// 'self'` needs no 'unsafe-inline' escape hatch - the CSP's XSS posture does not depend
/// on the page STAYING sink-free.
async fn session(State(g): State<Arc<Gate>>) -> impl IntoResponse {
    let body = format!("window.STYX_TOKEN=\"{}\";", g.token());
    ([(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "no-store")], harden(), body)
}

fn asset(body: &'static [u8], mime: &'static str) -> impl IntoResponse + use<> {
    ([(header::CONTENT_TYPE, mime)], harden(), body)
}

/// The page tier: assets behind the Host/Origin gate (no token - see the module doc).
pub fn ui_router(g: Arc<Gate>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/session.js", get(session))
        .route("/app.css", get(|| async { asset(CSS.as_bytes(), "text/css") }))
        .route("/app.js", get(|| async { asset(JS.as_bytes(), "text/javascript") }))
        .route("/favicon.svg", get(|| async { asset(FAVICON.as_bytes(), "image/svg+xml") }))
        .route("/fonts/didot-latin.woff2", get(|| async { asset(DIDOT_LATIN, "font/woff2") }))
        .route("/fonts/didot-greek.woff2", get(|| async { asset(DIDOT_GREEK, "font/woff2") }))
        .route("/fonts/plex-mono-400.woff2", get(|| async { asset(PLEX_400, "font/woff2") }))
        .route("/fonts/plex-mono-500.woff2", get(|| async { asset(PLEX_500, "font/woff2") }))
        .with_state(g.clone())
        .layer(axum::middleware::from_fn_with_state(g, page_gate))
}
