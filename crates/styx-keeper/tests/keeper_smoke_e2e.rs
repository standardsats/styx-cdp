//! The R4 acceptance: a wallet opens a vault, the oracles cheapen through the mock quote
//! transport, and the keeper - watching a FOREIGN vault whose owner it never configured
//! (the witness-scan recovery at work) - performs its duties in escalation order: poke,
//! refresh, partial liquidation at the dip, bad-debt closure at the crash.
//!
//! Ignored by default: needs ELEMENTSD_EXE (set inside `nix develop`). Run with
//! `cargo test -p styx-keeper -- --ignored`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::oracle::{OracleSlot, TickPayload};
use styx_core::units::{BlockHeight, Obol, Price, RatioK, Sats};
use styx_keeper::keeper::{Keeper, KeeperOpts, Performed};
use styx_node::regtest::{deploy, Deployment, SUPPLY};
use styx_wallet::config::WalletConfig;
use styx_wallet::wallet::Wallet;
use styx_watch::quotes::WireQuote;
use styx_watch::transport::{MockHub, QuoteTransport};

/// The same two-file config fabrication the wallet e2e uses, per role.
fn role_config(dep: &Deployment, dir: &std::path::Path, role: &str, key_seed: u8) -> WalletConfig {
    use styx_watch::config as net;
    let styxnet_path = dir.join("styxnet.toml");
    if !styxnet_path.exists() {
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
        net::StyxnetConfig {
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
        }
        .save(&styxnet_path)
        .unwrap();
    }
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
        dir.join(format!("{role}-snapshot.json")).display(),
        format!("{key_seed:02x}").repeat(32),
        format!("{:02x}", key_seed + 1).repeat(32),
    );
    WalletConfig::parse(&text).unwrap()
}

/// Publish a 3-slot quorum for the current tip into the hub (the oracles' side of the mock
/// transport).
async fn publish_quorum(dep: &Deployment, hub: &MockHub, price: u32) -> u32 {
    let h = dep.node.height().unwrap();
    let mut side = hub.endpoint();
    for slot in 0u8..3 {
        let payload = TickPayload {
            height: BlockHeight::new(h),
            price: Price::new(price),
            backing_k: RatioK::from_cr_percent(100),
        };
        let wire =
            WireQuote::sign(&dep.oracle_keys[slot as usize], OracleSlot::new(slot).unwrap(), &payload);
        side.publish(&wire).await.expect("publish");
    }
    h
}

/// One keeper pass fed from the transport: sync (the book windows against the indexed
/// tip), drain, assemble, step - the daemon loop's body.
async fn keeper_pass(
    keeper: &mut Keeper,
    endpoint: &mut styx_watch::transport::MockTransport,
) -> Option<Performed> {
    keeper.purse.sync().expect("sync");
    keeper.drain(endpoint).await;
    let tick = keeper.assemble().expect("quorum assembles");
    keeper.step(&tick).expect("step")
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn wallet_opens_oracles_cheapen_keeper_liquidates() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let dep = deploy(18_000_000);
        let dir = std::env::temp_dir().join(format!("styx-keeper-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // The borrower's wallet opens $40k against 1 BTC at $120k.
        let mut wallet =
            Wallet::open_session(&role_config(&dep, &dir, "wallet", 0x1a)).expect("wallet session");
        wallet.sync().unwrap();
        wallet.fund(200_000_000).expect("fund wallet");
        dep.node.mine().unwrap();
        wallet.sync().unwrap();
        let h = dep.node.height().unwrap();
        let opened = wallet
            .open(Obol::new(4_000_000), Sats::new(100_000_000), &dep.tick(h, 120_000))
            .expect("open");
        let vault_born = opened.vault.unwrap();
        dep.node.mine().unwrap();
        wallet.sync().unwrap();

        // The keeper's purse: its own keys - the wallet's owner is NOT among its candidates.
        let mut keeper = Keeper::new(
            Wallet::open_session(&role_config(&dep, &dir, "keeper", 0x2a)).expect("keeper session"),
            KeeperOpts { poke_lag: 2, refresh_lag: 3, walkback: 8 },
        );
        keeper.purse.fund(5_000_000).expect("keeper fee funds");
        dep.node.mine().unwrap();

        // The wallet hands its 4M OBOL principal to the keeper (the market, abridged) -
        // through the CLI-facing send op.
        wallet.send_obol(keeper.purse.funding_spk(), Obol::new(4_000_000)).expect("obol transfer");
        dep.node.mine().unwrap();

        // The keeper sees the foreign vault with a RESOLVED owner: the witness scan, not a
        // configured candidate (candidate A of the R4 decision).
        keeper.purse.sync().unwrap();
        let tracked = keeper.purse.state.vaults.get(&vault_born.outpoint).expect("tracked");
        assert_eq!(tracked.owner, Some(vault_born.state.owner), "owner out of the witness");

        let hub = MockHub::new();
        let mut endpoint = hub.endpoint();

        // Healthy price, lagging anchor: the poke duty fires first.
        dep.node.mine().unwrap();
        dep.node.mine().unwrap();
        publish_quorum(&dep, &hub, 120_000).await;
        let done = keeper_pass(&mut keeper, &mut endpoint).await;
        assert!(matches!(done, Some(Performed::Poked { .. })), "expected a poke, got {done:?}");

        // Same tick, NO block yet: one action per block - the pending poke sits out (the
        // anchor still reads old) and nothing else runs either (a second action would
        // fight the first over the purse's coins in the mempool).
        let done = keeper_pass(&mut keeper, &mut endpoint).await;
        assert_eq!(done, None, "one action per block");
        dep.node.mine().unwrap();

        // Anchor fresh now; the dormant healthy vault is next: the refresh duty (M-2).
        publish_quorum(&dep, &hub, 120_000).await;
        let done = keeper_pass(&mut keeper, &mut endpoint).await;
        assert!(
            matches!(done, Some(Performed::Refreshed { vault, .. }) if vault == vault_born.outpoint),
            "expected a refresh, got {done:?}"
        );
        dep.node.mine().unwrap();

        // The $50k dip lands the vault in the partial band: the keeper heals it.
        publish_quorum(&dep, &hub, 50_000).await;
        let done = keeper_pass(&mut keeper, &mut endpoint).await;
        let Some(Performed::Partial { dd, .. }) = done else {
            panic!("expected a partial liquidation, got {done:?}");
        };
        dep.node.mine().unwrap();
        keeper.purse.sync().unwrap();
        let healed = keeper
            .purse
            .state
            .vaults
            .iter()
            .find_map(|(op, v)| v.known(*op))
            .expect("the healed vault is tracked");
        assert_eq!(healed.state.debt, Obol::new(4_000_000 - dd.raw()));
        assert_eq!(healed.state.owner, vault_born.state.owner, "owner rides the recursion");

        // The $35k crash puts the healed vault under water: bad-debt closes it, the reserve
        // compensates the keeper.
        let reserve_before = keeper.purse.protocol().unwrap().reserve.value;
        let keeper_lbtc_before: u64 = keeper.purse.lbtc_coins().unwrap().iter().map(|(_, v)| v).sum();
        publish_quorum(&dep, &hub, 35_000).await;
        let done = keeper_pass(&mut keeper, &mut endpoint).await;
        assert!(
            matches!(done, Some(Performed::BadDebt { vault, .. }) if vault == healed.outpoint),
            "expected a bad-debt closure, got {done:?}"
        );
        dep.node.mine().unwrap();
        keeper.purse.sync().unwrap();

        // Every debt is burned: no vaults, the pot back at the full supply, the reserve
        // paid out, the keeper collected collateral plus the shortfall cover.
        assert!(keeper.purse.state.vaults.is_empty());
        assert!(keeper.purse.state.lost.is_empty());
        assert_eq!(keeper.purse.protocol().unwrap().pot.value.raw(), SUPPLY);
        assert!(keeper.purse.protocol().unwrap().reserve.value < reserve_before);
        let keeper_lbtc: u64 = keeper.purse.lbtc_coins().unwrap().iter().map(|(_, v)| v).sum();
        assert!(keeper_lbtc > keeper_lbtc_before, "the keeper was made whole in sats");
        assert_eq!(
            keeper.purse.obol_coins().unwrap().iter().map(|(_, v)| v).sum::<u64>(),
            0,
            "all keeper OBOL burned into the pot"
        );

        // A quiet pass does nothing.
        dep.node.mine().unwrap();
        publish_quorum(&dep, &hub, 35_000).await;
        let done = keeper_pass(&mut keeper, &mut endpoint).await;
        assert!(matches!(done, None | Some(Performed::Poked { .. })), "got {done:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    });
}
