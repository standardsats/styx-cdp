//! The shared conflict-retry policy off node: conflicts resync and rebuild (bounded), a
//! Rejected is an alert, and a vanished target after a conflict splits by policy - a lost
//! race for a keeper (benign), a stop-and-look for an owner (error).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::elements::hashes::Hash;
use styx_core::elements::{OutPoint, Txid};
use styx_core::units::BlockHeight;
use styx_node::BroadcastError;
use styx_pset::error::BuildError;
use styx_wallet::wallet::{retry_conflicts, LostRace, WalletError};

struct Sim {
    attempts: u32,
    resyncs: u32,
}

fn txid() -> Txid {
    Txid::from_slice(&[7u8; 32]).unwrap()
}

fn conflict() -> WalletError {
    WalletError::Broadcast(BroadcastError::Conflict("missingorspent".into()))
}

fn stale_ratchet() -> WalletError {
    WalletError::Build(BuildError::RatchetNotAdvanced {
        tick: BlockHeight::new(10),
        last_height: BlockHeight::new(10),
    })
}

#[test]
fn a_conflict_resyncs_and_the_rebuild_lands() {
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let out = retry_conflicts(
        "test",
        &mut sim,
        |s| {
            s.resyncs += 1;
            Ok(())
        },
        |s| {
            s.attempts += 1;
            if s.attempts == 1 {
                Err(conflict())
            } else {
                Ok(txid())
            }
        },
        LostRace::Benign,
    )
    .unwrap();
    assert_eq!(out, Some(txid()));
    assert_eq!((sim.attempts, sim.resyncs), (2, 1));
}

#[test]
fn persistent_conflicts_exhaust() {
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let err = retry_conflicts(
        "test",
        &mut sim,
        |_| Ok(()),
        |s| {
            s.attempts += 1;
            Err(conflict())
        },
        LostRace::Error,
    )
    .unwrap_err();
    assert!(matches!(err, WalletError::ConflictExhausted { attempts: 3, .. }));
    assert_eq!(sim.attempts, 3);
}

#[test]
fn a_vanished_target_after_a_conflict_splits_by_policy() {
    let vanish = |s: &mut Sim| -> Result<Txid, WalletError> {
        s.attempts += 1;
        if s.attempts == 1 {
            Err(conflict())
        } else {
            Err(WalletError::VaultNotFound(OutPoint::default()))
        }
    };

    // The keeper's view: someone else did the work - fine.
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let out = retry_conflicts("test", &mut sim, |_| Ok(()), vanish, LostRace::Benign).unwrap();
    assert_eq!(out, None, "resolved elsewhere, not an error");

    // The owner's view: my vault dissolved mid-op - that wants eyes.
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let err = retry_conflicts("test", &mut sim, |_| Ok(()), vanish, LostRace::Error).unwrap_err();
    assert!(matches!(err, WalletError::VaultNotFound(_)));

    // On the FIRST attempt the same error is a plain failure under either policy: nothing
    // raced us yet.
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let err = retry_conflicts(
        "test",
        &mut sim,
        |_| Ok(()),
        |_| Err(WalletError::VaultNotFound(OutPoint::default())),
        LostRace::Benign,
    )
    .unwrap_err();
    assert!(matches!(err, WalletError::VaultNotFound(_)));
}

#[test]
fn an_advanced_ratchet_after_a_conflict_follows_the_same_policy() {
    let stale = |s: &mut Sim| -> Result<Txid, WalletError> {
        s.attempts += 1;
        if s.attempts == 1 {
            Err(conflict())
        } else {
            Err(stale_ratchet())
        }
    };
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let out = retry_conflicts("test", &mut sim, |_| Ok(()), stale, LostRace::Benign).unwrap();
    assert_eq!(out, None, "refresh-shaped lost race");

    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let err = retry_conflicts("test", &mut sim, |_| Ok(()), stale, LostRace::Error).unwrap_err();
    assert!(matches!(err, WalletError::Build(BuildError::RatchetNotAdvanced { .. })));
}

#[test]
fn a_rejected_broadcast_is_an_alert_not_a_retry() {
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let err = retry_conflicts(
        "test",
        &mut sim,
        |_| Ok(()),
        |s| {
            s.attempts += 1;
            Err(WalletError::Broadcast(BroadcastError::Rejected("covenant".into())))
        },
        LostRace::Benign,
    )
    .unwrap_err();
    assert!(matches!(err, WalletError::Rejected { .. }));
    assert_eq!(sim.attempts, 1, "no retry on a builder-invariant break");
}
