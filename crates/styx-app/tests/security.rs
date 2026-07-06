//! The U0 security invariants, off node: the loopback-only listener, the session token,
//! and the Host/Origin allowlist. Each negative is the attack it names.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use styx_app::auth::{gate, Gate, TOKEN_HEADER};
use styx_app::config::{AppConfig, ConfigError};
use tower::ServiceExt;

fn v4() -> std::net::SocketAddr {
    "127.0.0.1:9780".parse().unwrap()
}

/// A gated stub router: the gate is what is under test, not the handlers behind it.
fn gated(g: Arc<Gate>) -> Router {
    Router::new()
        .route("/ping", get(|| async { "pong" }))
        .layer(axum::middleware::from_fn_with_state(g, gate))
}

fn request(host: &str, origin: Option<&str>, token: Option<&str>) -> Request<axum::body::Body> {
    let mut b = Request::builder().uri("/ping").header("host", host);
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    if let Some(t) = token {
        b = b.header(TOKEN_HEADER, t);
    }
    b.body(axum::body::Body::empty()).unwrap()
}

#[tokio::test]
async fn the_token_gates_every_call() {
    let g = Arc::new(Gate::mint(v4()));
    let token = g.token().to_string();
    let app = gated(g);

    let no_token = app.clone().oneshot(request("127.0.0.1:9780", None, None)).await.unwrap();
    assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);

    let wrong = app.clone().oneshot(request("127.0.0.1:9780", None, Some("deadbeef"))).await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let ok = app.oneshot(request("localhost:9780", None, Some(&token))).await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_foreign_host_is_the_dns_rebinding_shape_and_is_forbidden() {
    // A hostile page resolves wallet.evil.example to 127.0.0.1 and fetches: the socket is
    // local, but Host still carries the foreign name - even a leaked token must not help.
    let g = Arc::new(Gate::mint(v4()));
    let token = g.token().to_string();
    let app = gated(g);
    let resp = app.oneshot(request("wallet.evil.example:9780", None, Some(&token))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_cross_origin_call_is_forbidden_and_same_origin_passes() {
    let g = Arc::new(Gate::mint(v4()));
    let token = g.token().to_string();
    let app = gated(g);

    let cross = app
        .clone()
        .oneshot(request("127.0.0.1:9780", Some("http://evil.example"), Some(&token)))
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);

    // https on loopback is not this listener either: the scheme is part of the origin.
    let https = app
        .clone()
        .oneshot(request("127.0.0.1:9780", Some("https://127.0.0.1:9780"), Some(&token)))
        .await
        .unwrap();
    assert_eq!(https.status(), StatusCode::FORBIDDEN);

    let same = app
        .oneshot(request("127.0.0.1:9780", Some("http://127.0.0.1:9780"), Some(&token)))
        .await
        .unwrap();
    assert_eq!(same.status(), StatusCode::OK);
}

#[test]
fn a_non_loopback_listener_is_a_refused_config() {
    let toml = |listen: &str| {
        format!(
            r#"
styxnet = "/tmp/styxnet.toml"
rpc_url = "http://127.0.0.1:18884"
rpc_user = "styx"
rpc_password = "styx"
snapshot = "/tmp/snapshot.json"
owner_seckey = "{k}"
funding_seckey = "{k}"
listen = "{listen}"
"#,
            k = "11".repeat(32),
        )
    };
    assert!(matches!(AppConfig::parse(&toml("0.0.0.0:9780")), Err(ConfigError::NonLoopback(_))));
    assert!(matches!(AppConfig::parse(&toml("192.168.1.10:9780")), Err(ConfigError::NonLoopback(_))));
    assert!(AppConfig::parse(&toml("127.0.0.1:9780")).is_ok());
    // The IPv6 loopback is a loopback.
    assert!(AppConfig::parse(&toml("[::1]:9780")).is_ok());
}

#[test]
fn tokens_are_fresh_per_session() {
    assert_ne!(Gate::mint(v4()).token(), Gate::mint(v4()).token());
    assert_eq!(Gate::mint(v4()).token().len(), 64);
}

#[tokio::test]
async fn the_gate_answers_to_the_bound_address_verbatim() {
    // The reviewer-found shape: a [::1] listener must accept Host: [::1]:9780 - an
    // allowlist hardcoded to 127.0.0.1 serves nothing but 403s on an IPv6 loopback.
    let g = Arc::new(Gate::mint("[::1]:9780".parse().unwrap()));
    let token = g.token().to_string();
    let app = gated(g);

    let v6 = app.clone().oneshot(request("[::1]:9780", None, Some(&token))).await.unwrap();
    assert_eq!(v6.status(), StatusCode::OK);

    // The v4 form is NOT this listener; localhost:port always is.
    let v4 = app.clone().oneshot(request("127.0.0.1:9780", None, Some(&token))).await.unwrap();
    assert_eq!(v4.status(), StatusCode::FORBIDDEN);
    let local = app.oneshot(request("localhost:9780", None, Some(&token))).await.unwrap();
    assert_eq!(local.status(), StatusCode::OK);
}

#[test]
fn a_keeper_table_typo_is_a_refused_config() {
    // The flattened purse swallows unknown top-level keys by construction, but the keeper
    // table can and does refuse them: poke_lg must not silently mean "default poke_lag".
    let toml = format!(
        r#"
styxnet = "/tmp/styxnet.toml"
rpc_url = "http://127.0.0.1:18884"
rpc_user = "styx"
rpc_password = "styx"
snapshot = "/tmp/snapshot.json"
owner_seckey = "{k}"
funding_seckey = "{k}"

[keeper]
poke_lg = 2
"#,
        k = "11".repeat(32),
    );
    assert!(matches!(AppConfig::parse(&toml), Err(ConfigError::Toml(_))));
}
