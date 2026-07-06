//! The reindex anchor: run the full lifecycle on a real node, then rebuild the state from
//! genesis through the catch-up scan. The lifecycle driver's tracked trace is the ground
//! truth: the indexer must recognize every op at its txid and converge to the same final
//! state, through all the non-protocol noise (funding, merges, wallet change) a real chain
//! carries.
//!
//! Ignored by default: needs ELEMENTSD_EXE (set inside `nix develop`). Run with
//! `cargo test -p styx-watch -- --ignored`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_node::lifecycle;
use styx_node::regtest::deploy;
use styx_watch::index::{Event, IndexState};
use styx_watch::snapshot;
use styx_watch::sync::catch_up;

fn keypair(secret: u8) -> styx_core::elements::secp256k1_zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    styx_core::elements::secp256k1_zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn reindex_from_genesis_matches_the_lifecycle_trace() {
    let dep = deploy(18_000_000);
    let owner = keypair(10);
    let owners = [owner.x_only_public_key().0];
    let trace = lifecycle::run(&dep, &owner);

    // Reindex the whole chain from genesis.
    let mut state = IndexState::genesis(dep.node.genesis().unwrap());
    let notices = catch_up(&dep.node, &dep.ctx, &owners, &mut state).unwrap();

    // The final state converges to the smoke's tracked state: all vaults consumed, the
    // protocol snapshot equal field for field.
    let last = trace.last().unwrap();
    assert!(last.vaults.is_empty());
    assert!(state.vaults.is_empty(), "surviving vaults: {:?}", state.vaults);
    assert!(state.lost.is_empty(), "lost vaults: {:?}", state.lost);
    assert_eq!(state.protocol().unwrap(), last.protocol);

    // Every step of the trace was recognized as the right event at the right txid, and
    // every vault the step left alive is a vault the indexer tracked at that txid's
    // successor outpoint.
    for step in &trace {
        let at_txid: Vec<&Event> =
            notices.iter().filter(|n| n.txid == step.txid).map(|n| &n.event).collect();
        assert!(!at_txid.is_empty(), "step {} produced no events", step.label);
        let recognized = at_txid.iter().any(|e| match step.label {
            "poke" => matches!(e, Event::Poked { .. }),
            "open" => matches!(e, Event::Opened { owner: Some(pk), .. } if *pk == owners[0]),
            "repay" => matches!(e, Event::Repaid { .. }),
            "draw" => matches!(e, Event::Drawn { .. }),
            "refresh" => matches!(e, Event::Refreshed { .. }),
            // Partial liquidation and redemption are layout-isomorphic by design.
            "liquidate" | "redeem" => matches!(e, Event::Deleveraged { .. }),
            "bad_debt" => matches!(e, Event::BadDebtClosed { .. }),
            "full_liq" => matches!(e, Event::FullyLiquidated { .. }),
            "close" => matches!(e, Event::Closed { .. }),
            other => panic!("unknown step label {other}"),
        });
        assert!(recognized, "step {} not recognized in {at_txid:?}", step.label);
        for v in &step.vaults {
            if v.outpoint.txid != step.txid {
                continue; // untouched by this step; checked at its own birth/transition
            }
            let tracked = at_txid.iter().any(|e| match e {
                Event::Opened { vault, .. } => *vault == v.outpoint,
                Event::Repaid { successor, .. }
                | Event::Drawn { successor, .. }
                | Event::Refreshed { successor, .. }
                | Event::Deleveraged { successor, .. } => *successor == v.outpoint,
                _ => false,
            });
            assert!(tracked, "step {} vault {} not tracked", step.label, v.outpoint);
        }
    }

    // On-node dust and fragments: a foreign asset at the pot address is invisible, a
    // same-asset payment at the reserve address is a fragment report; neither moves the
    // snapshot. Resume from a saved snapshot to cover the catch-up path a daemon runs.
    let protocol_before = state.protocol().unwrap();
    let dir = std::env::temp_dir().join(format!("styx-reindex-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("snapshot.json");
    snapshot::save(&state, &path).unwrap();

    dep.node.fund_address(dep.ctx.params.policy, &dep.ctx.artifacts.pot_spk(), 1_000).unwrap();
    dep.node.fund_address(dep.ctx.params.policy, &dep.ctx.artifacts.stability_spk(), 500_000).unwrap();

    let mut resumed = snapshot::load(&path).unwrap();
    assert_eq!(resumed, state);
    let tail = catch_up(&dep.node, &dep.ctx, &owners, &mut resumed).unwrap();
    assert!(tail.iter().any(|n| matches!(n.event, Event::Fragment { role: "reserve" })));
    assert!(!tail.iter().any(|n| matches!(n.event, Event::Fragment { role: "pot" })));
    assert_eq!(resumed.protocol().unwrap(), protocol_before, "dust must not move the snapshot");

    // A fresh full rescan agrees with the resumed incremental one.
    let mut fresh = IndexState::genesis(dep.node.genesis().unwrap());
    catch_up(&dep.node, &dep.ctx, &owners, &mut fresh).unwrap();
    assert_eq!(fresh, resumed);
    std::fs::remove_dir_all(&dir).unwrap();
}
