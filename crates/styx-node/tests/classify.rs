//! The broadcast-error classification, pinned against the node error strings the on-node
//! e2e calibrates. With several keepers racing on minute-long testnet blocks, every
//! same-inputs contention shape must read as Conflict - a misread would alert-stop a
//! healthy daemon.

use styx_node::client::{classify_broadcast, BroadcastVerdict};

#[test]
fn every_contention_shape_is_a_conflict() {
    for msg in [
        "bad-txns-inputs-missingorspent",
        "txn-mempool-conflict",
        "insufficient fee, rejecting replacement 1a2b...",
        "conflict with tx in mempool",
    ] {
        assert_eq!(classify_broadcast(msg), BroadcastVerdict::Conflict, "{msg}");
    }
}

#[test]
fn a_known_transaction_is_idempotent_success() {
    for msg in ["Transaction already in block chain", "txn-already-known", "txn-already-in-mempool"] {
        assert_eq!(classify_broadcast(msg), BroadcastVerdict::AlreadyKnown, "{msg}");
    }
}

#[test]
fn genuine_refusals_stay_rejected() {
    for msg in [
        "non-mandatory-script-verify-flag (Script failed)",
        "min relay fee not met",
        "bad-txns-in-belowout",
    ] {
        assert_eq!(classify_broadcast(msg), BroadcastVerdict::Rejected, "{msg}");
    }
}
