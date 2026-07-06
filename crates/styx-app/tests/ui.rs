//! The U1 invariants, off node: the served assets reference nothing beyond this origin
//! (no CDN, no telemetry - asserted, then browser-enforced by the CSP), and the page tier
//! sits behind the Host/Origin gate.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::http::{Request, StatusCode};
use styx_app::auth::Gate;
use styx_app::ui::{all_assets, ui_router, CSP};
use tower::ServiceExt;

fn gate() -> Arc<Gate> {
    Arc::new(Gate::mint("127.0.0.1:9780".parse().unwrap()))
}

fn get(host: &str, path: &str) -> Request<axum::body::Body> {
    Request::builder().uri(path).header("host", host).body(axum::body::Body::empty()).unwrap()
}

#[test]
fn no_asset_references_an_external_url() {
    for (name, body) in all_assets() {
        assert!(!body.contains("http://"), "{name} carries an external http URL");
        assert!(!body.contains("https://"), "{name} carries an external https URL");
        assert!(!body.contains("url(//"), "{name} carries a protocol-relative URL");
    }
}

#[test]
fn the_csp_pins_every_source_to_self() {
    for directive in [
        "default-src 'none'",
        "connect-src 'self'",
        "font-src 'self'",
        "script-src 'self'",
        "frame-ancestors 'none'",
    ] {
        assert!(CSP.contains(directive), "CSP lost `{directive}`");
    }
    assert!(!CSP.contains("unsafe-inline"), "the CSP must stay inline-free");
}

#[tokio::test]
async fn the_page_tier_is_host_gated_and_carries_the_token() {
    let g = gate();
    let token = g.token().to_string();
    let app = ui_router(g);

    // The DNS-rebinding shape is refused before any byte of the page.
    let foreign = app.clone().oneshot(get("wallet.evil.example:9780", "/")).await.unwrap();
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);

    // The page itself is inline-free and token-free; the hardening headers are present.
    let page = app.clone().oneshot(get("127.0.0.1:9780", "/")).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers().get("content-security-policy").and_then(|v| v.to_str().ok()), Some(CSP));
    assert_eq!(page.headers().get("x-frame-options").and_then(|v| v.to_str().ok()), Some("DENY"));
    assert_eq!(
        page.headers().get("x-content-type-options").and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    let body = axum::body::to_bytes(page.into_body(), 1 << 20).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(!html.contains(&token), "the page must NOT embed the token inline");
    assert!(html.contains("/session.js"), "the page loads the token from its own origin");

    // The token travels in /session.js, same gate, never cached.
    let sess = app.clone().oneshot(get("127.0.0.1:9780", "/session.js")).await.unwrap();
    assert_eq!(sess.status(), StatusCode::OK);
    assert_eq!(sess.headers().get("cache-control").and_then(|v| v.to_str().ok()), Some("no-store"));
    let body = axum::body::to_bytes(sess.into_body(), 1 << 20).await.unwrap();
    assert!(String::from_utf8(body.to_vec()).unwrap().contains(&token));

    // Assets resolve; fonts are real woff2 payloads.
    for path in ["/app.css", "/app.js", "/fonts/didot-latin.woff2", "/fonts/plex-mono-400.woff2"] {
        let resp = app.clone().oneshot(get("localhost:9780", path)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
    }
}
