//! The U0 acceptance: the whole owner cycle driven through the HTTP API - the served
//! router, the gate, real keys, a real node. Ticks are set at the lib boundary (the test
//! is its own oracle, like every prune-tier fixture); the relay loop is the binary's job
//! and is proven by the wallet CLI path it reuses.
//!
//! Ignored by default: needs ELEMENTSD_EXE (set inside `nix develop`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::IntoFuture;
use std::sync::Arc;

use styx_app::api::{router, AppState};
use styx_app::auth::{Gate, TOKEN_HEADER};
use styx_node::regtest::{deploy, Deployment, SUPPLY};
use styx_wallet::config::WalletConfig;
use styx_wallet::wallet::Wallet;

fn wallet_config(dep: &Deployment, dir: &std::path::Path) -> WalletConfig {
    use styx_watch::config as net;
    let oracles = dep
        .oracle_keys
        .iter()
        .enumerate()
        .map(|(slot, k)| net::OracleEntry {
            slot: slot as u8,
            protocol_pk: k.x_only_public_key().0.to_string(),
            nostr_pk: None,
            admin_url: None,
        })
        .collect();
    let styxnet = net::StyxnetConfig {
        network: net::Network {
            chain: "elementsregtest".into(),
            genesis: Some(dep.ctx.genesis.to_string()),
        },
        assets: Some(net::Assets {
            obol: dep.ctx.params.obol.to_string(),
            issuer_token: dep.ctx.params.issuer_token.to_string(),
            policy: dep.ctx.params.policy.to_string(),
        }),
        protocol: Some(net::Protocol {
            issuer_anchor_genesis: dep.protocol.issuer.state.last_mint_height.raw(),
        }),
        oracles,
        nostr: net::Nostr { relays: vec![] },
    };
    let styxnet_path = dir.join("styxnet.toml");
    styxnet.save(&styxnet_path).unwrap();
    let text = format!(
        r#"
styxnet = "{}"
rpc_url = "{}/wallet/wallet"
rpc_cookie = "{}"
snapshot = "{}"
owner_seckey = "{}"
funding_seckey = "{}"
"#,
        styxnet_path.display(),
        dep.daemon.rpc_url(),
        dep.daemon.params().cookie_file.display(),
        dir.join("snapshot.json").display(),
        "1a".repeat(32),
        "1b".repeat(32),
    );
    WalletConfig::parse(&text).unwrap()
}

struct Api {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl Api {
    async fn get(&self, path: &str) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .header(TOKEN_HEADER, &self.token)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> (u16, String) {
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .header(TOKEN_HEADER, &self.token)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        (resp.status().as_u16(), resp.text().await.unwrap())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
async fn full_owner_cycle_through_the_http_api() {
    let dep = deploy(18_000_000);
    let dir = std::env::temp_dir().join(format!("styx-app-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = wallet_config(&dep, &dir);

    let wallet = Wallet::open_session(&cfg).expect("session");
    let state = AppState::new(
        wallet,
        styx_keeper::keeper::KeeperOpts::default(),
        std::time::Duration::from_millis(200),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = Arc::new(Gate::mint(addr));
    let api = Api {
        base: format!("http://{addr}"),
        token: gate.token().to_string(),
        client: reqwest::Client::new(),
    };
    tokio::spawn(axum::serve(listener, router(state.clone(), gate)).into_future());

    // The gate holds on the real socket too: no token, no service.
    let bare = api.client.get(format!("{}/api/status", api.base)).send().await.unwrap();
    assert_eq!(bare.status().as_u16(), 401);

    // Fund the wallet's one script straight on chain; the API reports it after a block.
    let (_, st) = api.get("/api/status").await;
    let address: styx_core::elements::Address = st["funding_address"].as_str().unwrap().parse().unwrap();
    dep.node
        .fund_address(dep.ctx.params.policy, &address.script_pubkey(), 200_000_000)
        .expect("funding broadcast");
    dep.node.mine().unwrap();
    let (_, st) = api.get("/api/status").await;
    assert_eq!(st["lbtc_sats"].as_u64().unwrap(), 200_000_000);
    assert_eq!(st["protocol"]["pot_units"].as_u64().unwrap(), SUPPLY);

    // Tickless ops are a 409-refusal, not a failure.
    let (code, body) = api.post("/api/open", serde_json::json!({"principal": 4_000_000})).await;
    assert_eq!((code, body.contains("no oracle quorum")), (409, true));

    // An undercollateralized open carries the builder's words through the API.
    let h = dep.node.height().unwrap();
    state.set_tick(dep.tick(h, 120_000));
    let (code, body) =
        api.post("/api/open", serde_json::json!({"principal": 4_000_000, "collateral": 1_000})).await;
    assert_eq!((code, body.contains("undercollateralized")), (409, true));
    // The refusal left no trace: no shaping transaction sits in the mempool (the
    // validate-first invariant in ops::open).
    let mempool = dep.node.rpc("getrawmempool", &[]).unwrap();
    assert_eq!(mempool.as_array().map(|a| a.len()), Some(0), "refusal must not shape: {mempool}");

    // OPEN 4M OBOL against 1 BTC (room for the draw below), shaped funding inside.
    let (code, body) = api
        .post("/api/open", serde_json::json!({"principal": 4_000_000, "collateral": 100_000_000}))
        .await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();
    let (_, st) = api.get("/api/status").await;
    assert_eq!(st["vaults"].as_array().unwrap().len(), 1);
    assert_eq!(st["obol_units"].as_u64().unwrap(), 4_000_000);

    // REPAY 1M.
    let (code, body) = api.post("/api/repay", serde_json::json!({"amount": 1_000_000})).await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();

    // DRAW 1M back at a fresher tick.
    let h = dep.node.height().unwrap();
    state.set_tick(dep.tick(h, 120_000));
    let (code, body) = api.post("/api/draw", serde_json::json!({"amount": 1_000_000})).await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();

    // REFRESH advances the ratchet.
    let h = dep.node.height().unwrap();
    state.set_tick(dep.tick(h, 120_000));
    let (code, body) = api.post("/api/refresh", serde_json::json!({})).await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();

    // REDEEM 0.5M.
    let h = dep.node.height().unwrap();
    state.set_tick(dep.tick(h, 120_000));
    let (code, body) = api.post("/api/redeem", serde_json::json!({"amount": 500_000})).await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();

    // CLOSE repays the remaining 3.5M and frees the collateral.
    let (code, body) = api.post("/api/close", serde_json::json!({})).await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();

    // Every debt came home, through HTTP only: no vaults, no OBOL, the pot at full supply.
    let (_, st) = api.get("/api/status").await;
    assert_eq!(st["vaults"].as_array().unwrap().len(), 0);
    assert_eq!(st["obol_units"].as_u64().unwrap(), 0);
    assert_eq!(st["protocol"]["pot_units"].as_u64().unwrap(), SUPPLY);
    assert!(st["lbtc_sats"].as_u64().unwrap() > 190_000_000, "collateral came back");
    std::fs::remove_dir_all(&dir).unwrap();
}
