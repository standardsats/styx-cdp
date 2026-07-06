//! The second U0 acceptance: keeper mode through the API - start the loop, dip the price,
//! watch a partial liquidation land on the shared purse, stop the loop.
//!
//! Ignored by default: needs ELEMENTSD_EXE (run inside `nix develop`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;

use styx_app::api::{router, AppState};
use styx_app::auth::{Gate, TOKEN_HEADER};
use styx_keeper::keeper::KeeperOpts;
use styx_node::regtest::{deploy, Deployment};
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

async fn post(api: &(String, String), path: &str, body: serde_json::Value) -> (u16, String) {
    let resp = reqwest::Client::new()
        .post(format!("{}{path}", api.0))
        .header(TOKEN_HEADER, &api.1)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

async fn status(api: &(String, String)) -> serde_json::Value {
    let resp = reqwest::Client::new()
        .get(format!("{}/api/status", api.0))
        .header(TOKEN_HEADER, &api.1)
        .send()
        .await
        .unwrap();
    serde_json::from_str(&resp.text().await.unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
async fn the_keeper_mode_heals_a_dip_through_the_api() {
    let dep = deploy(18_000_000);
    let dir = std::env::temp_dir().join(format!("styx-app-keeper-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg: WalletConfig = wallet_config(&dep, &dir);

    let wallet = Wallet::open_session(&cfg).expect("session");
    let state = AppState::new(
        wallet,
        KeeperOpts { poke_lag: 2, refresh_lag: 32, walkback: 8 },
        Duration::from_millis(150),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = Arc::new(Gate::mint(addr));
    tokio::spawn(axum::serve(listener, router(state.clone(), gate)).into_future());

    // Bootstrap the token the way a browser does: the page loads /session.js from the
    // same origin (inline-free, so the CSP carries no unsafe-inline escape hatch).
    let session = reqwest::Client::new()
        .get(format!("http://{addr}/session.js"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let marker = "window.STYX_TOKEN=\"";
    let at = session.find(marker).expect("session.js carries the token") + marker.len();
    let token = session[at..at + 64].to_string();
    let api = (format!("http://{addr}"), token);

    // Fund and open 4M against 1 BTC at $120k through the API (the owner side).
    let st = status(&api).await;
    let address: styx_core::elements::Address = st["funding_address"].as_str().unwrap().parse().unwrap();
    dep.node.fund_address(dep.ctx.params.policy, &address.script_pubkey(), 200_000_000).unwrap();
    dep.node.mine().unwrap();
    let h = dep.node.height().unwrap();
    state.set_tick(dep.tick(h, 120_000));
    let (code, body) =
        post(&api, "/api/open", serde_json::json!({"principal": 4_000_000, "collateral": 100_000_000}))
            .await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();

    // Keeper on. The dip to $50k puts the vault at ~125% CR: the partial band.
    let (code, body) = post(&api, "/api/keeper/start", serde_json::json!({})).await;
    assert_eq!((code, body.as_str()), (200, "running"));
    let (code, body) = post(&api, "/api/keeper/start", serde_json::json!({})).await;
    assert_eq!((code, body.contains("already running")), (400, true), "{body}");

    dep.node.mine().unwrap();
    let h = dep.node.height().unwrap();
    state.set_tick(dep.tick(h, 50_000));

    // The loop heals the vault on the shared purse: debt drops below the opened 4M.
    let mut healed = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        dep.node.mine().unwrap();
        let st = status(&api).await;
        assert!(st["keeper"].as_str().unwrap() != "idle", "keeper died: {st}");
        let vaults = st["vaults"].as_array().unwrap();
        if let Some(v) = vaults.first() {
            let debt = v["debt_units"].as_u64().unwrap();
            if debt < 4_000_000 {
                healed = Some(debt);
                break;
            }
        }
    }
    let healed = healed.expect("the keeper healed the dip");
    assert!(healed > 0, "partial, not a close");

    // Stop: the loop drains back to idle and the API stays up.
    let (code, _) = post(&api, "/api/keeper/stop", serde_json::json!({})).await;
    assert_eq!(code, 200);
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if status(&api).await["keeper"].as_str().unwrap() == "idle" {
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
    }
    panic!("the keeper loop did not stop");
}
