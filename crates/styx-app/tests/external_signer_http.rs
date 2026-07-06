//! The U3 acceptance: the external-signer path through the API. Open a vault, then CLOSE it
//! with the owner key kept OFF the app - export the digest, sign it out of band, apply the
//! signature, and the vault closes. This is the M6/M10 seam reaching the UI.
//!
//! Ignored by default: needs ELEMENTSD_EXE (run inside `nix develop`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::IntoFuture;
use std::sync::Arc;

use styx_app::api::{router, AppState};
use styx_app::auth::{Gate, TOKEN_HEADER};
use styx_core::elements::secp256k1_zkp as zkp;
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

/// The owner keypair the wallet config derives (owner_seckey = 0x1a * 32).
fn owner_key() -> zkp::Keypair {
    let sk = [0x1au8; 32];
    zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

fn sign_digest(kp: &zkp::Keypair, hex: &str) -> String {
    let mut d = [0u8; 32];
    for (i, b) in d.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    let msg = zkp::Message::from_digest(d);
    styx_core::secp().sign_schnorr_no_aux_rand(&msg, kp).to_string()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
async fn a_vault_closes_with_an_off_machine_owner_signature() {
    let dep = deploy(18_000_000);
    let dir = std::env::temp_dir().join(format!("styx-app-ext-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg: WalletConfig = wallet_config(&dep, &dir);

    let wallet = Wallet::open_session(&cfg).expect("session");
    let state = AppState::new(wallet, KeeperOpts::default(), std::time::Duration::from_millis(200));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = Arc::new(Gate::mint(addr));
    let api = (format!("http://{addr}"), gate.token().to_string());
    tokio::spawn(axum::serve(listener, router(state.clone(), gate)).into_future());

    // Fund and open 4M against 1 BTC.
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

    // Export the CLOSE for external signing. The response is the owner digest and key.
    let (code, body) = post(&api, "/api/export", serde_json::json!({"op": "close"})).await;
    assert_eq!(code, 200, "{body}");
    let req: serde_json::Value = serde_json::from_str(&body).unwrap();
    let digest = req["sighash"].as_str().unwrap();

    // A wrong-key signature is refused before broadcast.
    let wrong = zkp::Keypair::from_seckey_slice(styx_core::secp(), &[0x77u8; 32]).unwrap();
    let (code, _) = post(
        &api,
        "/api/apply",
        serde_json::json!({"txid": req["txid"], "owner_sig": sign_digest(&wrong, digest)}),
    )
    .await;
    assert_eq!(code, 400, "a stranger's signature must not apply");

    // The bad apply left the pending plan intact: the real owner key signs the SAME digest,
    // off the app, and it lands.
    let sig = sign_digest(&owner_key(), digest);
    let (code, body) =
        post(&api, "/api/apply", serde_json::json!({"txid": req["txid"], "owner_sig": sig})).await;
    assert_eq!(code, 200, "{body}");
    dep.node.mine().unwrap();

    // The vault is gone; the collateral came home to the funding spk.
    let st = status(&api).await;
    assert_eq!(st["vaults"].as_array().unwrap().len(), 0);
    assert!(st["lbtc_sats"].as_u64().unwrap() > 90_000_000, "collateral returned");
    std::fs::remove_dir_all(&dir).unwrap();
}
