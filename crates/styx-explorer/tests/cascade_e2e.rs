//! The U2 acceptance: the explorer renders a real chain's crash cascade. The lifecycle
//! driver produces the ground truth; the explorer indexes the same chain candidate-free
//! (every vault opaque, which is all a public view needs) and must tell the story back.
//!
//! Ignored by default: needs ELEMENTSD_EXE (run inside `nix develop`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_explorer::render::page;
use styx_explorer::state::ExplorerState;
use styx_node::lifecycle;
use styx_node::regtest::{deploy, SUPPLY};
use styx_watch::index::IndexState;
use styx_watch::sync::catch_up;

fn keypair(secret: u8) -> styx_core::elements::secp256k1_zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    styx_core::elements::secp256k1_zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn the_explorer_tells_the_cascade_back() {
    let dep = deploy(18_000_000);
    let owner = keypair(10);
    let trace = lifecycle::run(&dep, &owner);

    // Index the whole chain the way the explorer does: no owner candidates at all.
    let state = ExplorerState::new(IndexState::genesis(dep.node.genesis().unwrap()), None);
    let notices = {
        let mut guard = state.index.write().unwrap();
        catch_up(&dep.node, &dep.ctx, &[], &mut guard).unwrap()
    };
    state.push_events(notices);

    // The cascade's final truth, from the trace.
    let last = trace.last().unwrap();
    assert!(last.vaults.is_empty());

    let view = state.view();
    assert_eq!(view.vaults.len(), 0, "every vault consumed: {:?}", view.vaults.len());
    assert_eq!(view.lost, 0);
    assert_eq!(view.protocol.as_ref().unwrap().pot_units, SUPPLY);

    // The feed tells the story: birth, opens, the liquidation ladder, the closes.
    let feed = view.events.iter().map(|e| e.what.as_str()).collect::<Vec<_>>().join("\n");
    for chapter in ["Opened", "Deleveraged", "FullyLiquidated", "BadDebtClosed", "Closed"] {
        assert!(feed.contains(chapter), "the feed lost the {chapter} chapter:\n{feed}");
    }

    // And the page renders it without a tick (prices are the relay's job, absent here).
    let html = page(&view, None);
    assert!(html.contains("no quorum"));
    assert!(html.contains(&format!("{}", SUPPLY / 1_000_000))); // the pot line, formatted
}
