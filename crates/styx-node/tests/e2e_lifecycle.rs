//! The on-node acceptance suite: the full protocol lifecycle on a Simplicity regtest
//! elementsd (via the `lifecycle` driver, which carries the negative spot-checks that
//! calibrate "prune verdict == node verdict" for the pinned simplicityhl rev - re-run these
//! on any rev bump), plus scanner-refusal and broadcast-classification checks.
//!
//! Ignored by default: needs ELEMENTSD_EXE (set inside `nix develop`). Run with
//! `cargo test -p styx-node -- --ignored`.

use styx_core::elements::OutPoint;
use styx_core::units::Obol;
use styx_core::units::Sats;
use styx_node::client::{op_true, FEE};
use styx_node::regtest::{deploy, deploy_fragmented, SUPPLY};
use styx_node::scan::scan_protocol;
use styx_node::{lifecycle, BroadcastError};
use styx_pset::build;
use styx_pset::finalize::finalize;
use styx_pset::intent::{FundingCoin, PokeIntent};

fn keypair(secret: u8) -> styx_core::elements::secp256k1_zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    styx_core::elements::secp256k1_zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn full_lifecycle_smoke() {
    let dep = deploy(18_000_000);
    let trace = lifecycle::run(&dep, &keypair(10));

    // Every debt is repaid: no vault survives, the pot is back to the full supply, and the
    // scanner agrees with the tracked state (the calibration of scan_protocol against the
    // delta bookkeeping).
    let last = trace.last().expect("steps");
    assert!(last.vaults.is_empty());
    assert_eq!(last.protocol.pot.value, Obol::new(SUPPLY));
    let scanned = scan_protocol(&dep.node, &dep.ctx, last.protocol.issuer.state).expect("scan");
    assert_eq!(scanned.pot, last.protocol.pot);
    assert_eq!(scanned.reserve, last.protocol.reserve);
    assert_eq!(scanned.issuer, last.protocol.issuer);
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn ceremony_funds_a_reserve_from_a_fragmented_wallet() {
    // Faucet drips leave the wallet with many small coins and no single coin big enough for
    // the reserve. Multi-coin selection must fund the whole ceremony anyway; the old
    // biggest-coin split failed here with bad-txns-vout-negative.
    let dep = deploy_fragmented(2_000_000);
    assert_eq!(dep.protocol.reserve.value, Sats::new(2_000_000));
    let scanned = scan_protocol(&dep.node, &dep.ctx, dep.protocol.issuer.state).expect("scan");
    assert_eq!(scanned.reserve, dep.protocol.reserve);
    assert_eq!(scanned.pot, dep.protocol.pot);
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn scanner_refuses_a_fragmented_reserve_and_ignores_dust() {
    let dep = deploy(1_000_000);

    // Foreign-asset dust at the pot and issuer addresses: a griefer can always send it, and
    // the scanner's asset filter must keep the snapshot identical.
    dep.node.fund_address(dep.ctx.params.policy, &dep.ctx.artifacts.pot_spk(), 1_000).expect("pot dust");
    dep.node.mine().expect("confirm pot dust"); // the next fund only sees confirmed coins
    dep.node
        .fund_address(
            dep.ctx.params.policy,
            &dep.ctx.artifacts.issuer_spk(&dep.protocol.issuer.state),
            1_000,
        )
        .expect("issuer dust");
    dep.node.mine().expect("confirm dust"); // fund_address broadcasts only now
    let scanned = scan_protocol(&dep.node, &dep.ctx, dep.protocol.issuer.state).expect("scan");
    assert_eq!(scanned, dep.protocol, "dust must not move the snapshot");

    // L-3: a second same-asset UTXO at the reserve's constant address must stop the builders.
    dep.node
        .fund_address(dep.ctx.params.policy, &dep.ctx.artifacts.stability_spk(), 500_000)
        .expect("fragment");
    dep.node.mine().expect("confirm fragment");
    match scan_protocol(&dep.node, &dep.ctx, dep.protocol.issuer.state) {
        Err(styx_node::ScanError::ReserveFragmented { count: 2 }) => {}
        other => panic!("expected ReserveFragmented, got {other:?}"),
    }
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn issuer_respend_is_classified_as_conflict() {
    // Two pokes built from the same issuer UTXO: the second broadcast is the singleton
    // contention mode and must classify as Conflict, not Rejected.
    let dep = deploy(1_000_000);
    let coins = dep.node.fund_optrue(dep.ctx.params.policy, &[1_000_000, 1_000_000]).expect("fees");
    let h = dep.node.height().unwrap();
    let poke = |funding: OutPoint| {
        build::poke::poke(
            &dep.ctx,
            &dep.protocol.issuer,
            &PokeIntent {
                tick: dep.tick(h, 120_000),
                funding: FundingCoin { outpoint: funding, value: Sats::new(1_000_000), spk: op_true() },
                change_spk: op_true(),
                fee: FEE,
            },
        )
        .expect("builds")
    };
    let first = finalize(&dep.ctx, &poke(coins[0]).plan).expect("prune accepts");
    let second = finalize(&dep.ctx, &poke(coins[1]).plan).expect("prune accepts");
    dep.node.send_and_mine(&first).expect("first poke lands");
    match dep.node.send(&second) {
        Err(BroadcastError::Conflict(_)) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
    // Broadcast is idempotent: re-sending a transaction the chain already has is our own
    // txid, not a rejection (daemons polling faster than blocks confirm rebuild
    // deterministically and hit this constantly).
    assert_eq!(dep.node.send(&first).expect("idempotent"), first.txid());
}
