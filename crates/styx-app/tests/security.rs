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
    req_with(host, origin, token, None)
}

fn req_with(
    host: &str,
    origin: Option<&str>,
    token: Option<&str>,
    fetch_site: Option<&str>,
) -> Request<axum::body::Body> {
    let mut b = Request::builder().uri("/ping").header("host", host);
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    if let Some(fs) = fetch_site {
        b = b.header("sec-fetch-site", fs);
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

#[test]
fn the_listen_env_override_still_obeys_the_loopback_rule() {
    // The desktop shell sets STYX_APP_LISTEN to pick an ephemeral loopback port; it must
    // not be an escape hatch to a public bind.
    let cfg = AppConfig::parse(&format!(
        r#"
styxnet = "/tmp/s.toml"
rpc_url = "http://127.0.0.1:18884"
rpc_user = "styx"
rpc_password = "styx"
snapshot = "/tmp/s.json"
owner_seckey = "{k}"
funding_seckey = "{k}"
"#,
        k = "11".repeat(32),
    ))
    .unwrap();
    assert_eq!(cfg.resolve_listen(Some("127.0.0.1:9781")).unwrap().port(), 9781);
    assert!(matches!(cfg.resolve_listen(Some("0.0.0.0:9781")), Err(ConfigError::NonLoopback(_))));
    // No env: the config's own listen.
    assert_eq!(cfg.resolve_listen(None).unwrap().port(), 9780);
}

#[tokio::test]
async fn a_proxy_gate_answers_to_its_configured_hosts_and_origins() {
    // The Umbrel / StartOS relaxation, matched to what the platforms actually send: Umbrel
    // LAN over plain http (host carries the port), a Tor onion over plain http. The scheme
    // is configured, not inferred - requiring https would 403 both of these.
    use styx_app::auth::Gate;
    let g = Arc::new(Gate::mint_proxied(
        "0.0.0.0:9780".parse().unwrap(),
        vec!["umbrel.local:9780".to_string(), "abc.onion".to_string()],
        vec!["http://umbrel.local:9780".to_string(), "http://abc.onion".to_string()],
    ));
    let token = g.token().to_string();
    let app = gated(g);

    // Umbrel LAN: Host with port, http Origin - both configured -> served.
    let umbrel = app
        .clone()
        .oneshot(request("umbrel.local:9780", Some("http://umbrel.local:9780"), Some(&token)))
        .await
        .unwrap();
    assert_eq!(umbrel.status(), StatusCode::OK);

    // Tor onion, plain http -> served.
    let onion =
        app.clone().oneshot(request("abc.onion", Some("http://abc.onion"), Some(&token))).await.unwrap();
    assert_eq!(onion.status(), StatusCode::OK);

    // A foreign origin is forbidden even with a good Host (the allowlist is exact, whole).
    let cross = app
        .clone()
        .oneshot(request("umbrel.local:9780", Some("http://evil.example"), Some(&token)))
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);

    // A host that is not configured is forbidden.
    let other =
        app.oneshot(request("evil.local", Some("http://evil.local"), Some(&token))).await.unwrap();
    assert_eq!(other.status(), StatusCode::FORBIDDEN);
}

#[test]
fn the_proxy_section_is_the_only_way_past_the_loopback_rule() {
    use styx_app::config::AppConfig;
    let toml = |extra: &str| {
        format!(
            r#"
styxnet = "/tmp/s.toml"
rpc_url = "http://127.0.0.1:18884"
rpc_user = "styx"
rpc_password = "styx"
snapshot = "/tmp/s.json"
owner_seckey = "{k}"
funding_seckey = "{k}"
{extra}
"#,
            k = "11".repeat(32),
        )
    };
    // Without [proxy], a public listen is refused (unchanged).
    assert!(AppConfig::parse(&toml("listen = \"0.0.0.0:9780\"")).is_err());
    // With [proxy], the non-loopback bind is the explicit, opt-in relaxation.
    let cfg = AppConfig::parse(&toml(
        "[proxy]\nbind = \"0.0.0.0:9780\"\nallow_hosts = [\"umbrel.local:9780\"]\n\
         allow_origins = [\"http://umbrel.local:9780\"]",
    ))
    .unwrap();
    let (addr, hosts, origins) = cfg.bind().unwrap();
    assert!(!addr.ip().is_loopback());
    assert_eq!(hosts, vec!["umbrel.local:9780".to_string()]);
    assert_eq!(origins, vec!["http://umbrel.local:9780".to_string()]);
    // A garbage bind is still a config error.
    assert!(AppConfig::parse(&toml("[proxy]\nbind = \"not-an-address\"")).is_err());
}

#[tokio::test]
async fn a_cross_site_fetch_is_forbidden_even_without_an_origin() {
    // The token-theft shape: a hostile page does <script src=".../session.js">. That GET
    // carries the right Host and NO Origin (so the Origin check waves it through), but the
    // browser stamps it Sec-Fetch-Site: cross-site. It must be refused, or the token leaks.
    let g = Arc::new(Gate::mint(v4()));
    let token = g.token().to_string();
    let app = gated(g);

    let cross = app
        .clone()
        .oneshot(req_with("127.0.0.1:9780", None, Some(&token), Some("cross-site")))
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);

    // same-origin (the real page's fetch) and none (a typed URL) pass; absent (curl) passes.
    for site in ["same-origin", "none"] {
        let ok = app
            .clone()
            .oneshot(req_with("127.0.0.1:9780", None, Some(&token), Some(site)))
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK, "sec-fetch-site {site}");
    }
}

#[tokio::test]
async fn a_non_loopback_bind_does_not_auto_trust_localhost() {
    // On a proxy (non-loopback) bind the port is on a wider network; a `Host: localhost:port`
    // from an attacker must NOT be trusted (it would otherwise be handed the token). Only the
    // explicit allow_hosts count there.
    use styx_app::auth::Gate;
    let g = Gate::mint_proxied(
        "0.0.0.0:9780".parse().unwrap(),
        vec!["umbrel.local:9780".to_string()],
        vec!["http://umbrel.local:9780".to_string()],
    );
    let token = g.token().to_string();
    let app = gated(std::sync::Arc::new(g));

    for host in ["localhost:9780", "0.0.0.0:9780", "127.0.0.1:9780"] {
        let resp = app.clone().oneshot(request(host, None, Some(&token))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "host {host} must not be trusted");
    }
    // The configured proxy host still works.
    let ok = app.oneshot(request("umbrel.local:9780", None, Some(&token))).await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
}
