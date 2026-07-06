//! The R3 acceptance: a full owner cycle - fund, open (with the exact-funding shaping
//! self-spend), repay, draw, refresh, redeem, close - through the wallet library against a
//! real node, with real key-path p2tr coins signed via the PSET pipeline and ticks injected
//! from the test oracle keys (the mock quote source). The wallet's view comes exclusively
//! from its own indexer sync + coin scans, exactly as the CLI runs it.
//!
//! Ignored by default: needs ELEMENTSD_EXE (set inside `nix develop`). Run with
//! `cargo test -p styx-wallet -- --ignored`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::units::{Obol, Sats};
use styx_node::regtest::{deploy, Deployment, SUPPLY};
use styx_wallet::config::WalletConfig;
use styx_wallet::wallet::Wallet;

/// Fabricate the two config files the CLI would be given, pointing at the deployment.
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

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn full_owner_cycle_through_the_wallet() {
    let dep = deploy(18_000_000);
    let dir = std::env::temp_dir().join(format!("styx-wallet-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = wallet_config(&dep, &dir);

    let mut w = Wallet::open_session(&cfg).expect("session");
    w.sync().expect("sync");
    assert!(w.protocol().is_ok(), "the ceremony is indexed from genesis");
    assert!(w.my_vaults().is_empty());

    // Fund the wallet's p2tr key from the node wallet (fund_address mines its own block).
    w.fund(200_000_000).expect("fund");
    w.sync().unwrap();
    assert_eq!(w.lbtc_coins().unwrap().len(), 1);

    // OPEN $50k against 1 BTC: no exact coin exists, so the shaping self-spend kicks in and
    // the open chains on it in the mempool.
    let h = dep.node.height().unwrap();
    let report =
        w.open(Obol::new(5_000_000), Sats::new(100_000_000), &dep.tick(h, 120_000)).expect("open");
    dep.node.mine().unwrap();
    w.sync().unwrap();
    let vault = report.vault.unwrap();
    assert_eq!(w.my_vaults(), vec![vault]);
    assert_eq!(vault.state.debt, Obol::new(5_000_000));
    assert_eq!(vault.value, Sats::new(100_000_000));
    // The principal landed at the funding spk as an OBOL coin.
    assert_eq!(w.obol_coins().unwrap().iter().map(|(_, v)| v).sum::<u64>(), 5_000_000);

    // REPAY $20k from the principal coin; the surplus returns as change.
    let report = w.repay(None, Obol::new(2_000_000)).expect("repay");
    dep.node.mine().unwrap();
    w.sync().unwrap();
    let vault = report.vault.unwrap();
    assert_eq!(vault.state.debt, Obol::new(3_000_000));
    assert_eq!(w.my_vaults(), vec![vault]);
    assert_eq!(w.obol_coins().unwrap().iter().map(|(_, v)| v).sum::<u64>(), 3_000_000);

    // DRAW $10k more.
    let h = dep.node.height().unwrap();
    let report = w.draw(None, Obol::new(1_000_000), &dep.tick(h, 120_000)).expect("draw");
    dep.node.mine().unwrap();
    w.sync().unwrap();
    let vault = report.vault.unwrap();
    assert_eq!(vault.state.debt, Obol::new(4_000_000));
    assert_eq!(w.my_vaults(), vec![vault]);

    // REFRESH: the ratchet advances to the fresh tick.
    let h = dep.node.height().unwrap();
    let report = w.refresh(None, &dep.tick(h, 120_000)).expect("refresh");
    dep.node.mine().unwrap();
    w.sync().unwrap();
    let vault = report.vault.unwrap();
    assert_eq!(vault.state.last_height.raw(), h);
    assert_eq!(w.my_vaults(), vec![vault]);

    // REDEEM $5k against our own vault at par. The redeemer coin is the 1M draw coin, so
    // 0.5M comes back as OBOL change - deliberately fragmenting the wallet.
    let h = dep.node.height().unwrap();
    let report = w.redeem(None, Obol::new(500_000), &dep.tick(h, 120_000)).expect("redeem");
    dep.node.mine().unwrap();
    w.sync().unwrap();
    let vault = report.vault.unwrap();
    assert_eq!(vault.state.debt, Obol::new(3_500_000));

    // CLOSE needs a single 3.5M OBOL coin but the wallet holds {3M, 0.5M}: solvent yet
    // fragmented, so the consolidation self-spend runs first and the close chains on it.
    let obol = w.obol_coins().unwrap();
    assert_eq!(obol.len(), 2, "the fragmented shape this exercises: {obol:?}");
    assert!(obol.iter().all(|(_, v)| *v < 3_500_000));
    w.close(None).expect("close");
    dep.node.mine().unwrap();
    w.sync().unwrap();
    assert!(w.my_vaults().is_empty());
    assert!(w.state.lost.is_empty());
    assert_eq!(w.obol_coins().unwrap().iter().map(|(_, v)| v).sum::<u64>(), 0);

    // Every debt came home: the pot holds the full supply again, and the wallet's sats are
    // its funding minus the tx fees and the two borrow fees it paid.
    assert_eq!(w.protocol().unwrap().pot.value.raw(), SUPPLY);
    let sats: u64 = w.lbtc_coins().unwrap().iter().map(|(_, v)| v).sum();
    assert!(sats > 190_000_000, "collateral came back (have {sats})");

    // A fresh session from the persisted snapshot agrees.
    let mut again = Wallet::open_session(&cfg).expect("session from snapshot");
    let notices = again.sync().unwrap();
    assert!(notices.is_empty(), "nothing new after the sealed tip");
    assert_eq!(again.state, w.state);
    std::fs::remove_dir_all(&dir).unwrap();
}
