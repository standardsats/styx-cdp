//! The conflict-retry policy off node: conflicts resync and rebuild (bounded), a vanished
//! target after a conflict is a lost race (fine), a Rejected is an alert, everything else
//! propagates.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::elements::hashes::Hash;
use styx_core::elements::{OutPoint, Txid};
use styx_keeper::keeper::{retry_conflicts, KeeperError};
use styx_node::BroadcastError;
use styx_wallet::wallet::WalletError;

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
    )
    .unwrap_err();
    assert!(matches!(err, KeeperError::ConflictExhausted { attempts: 3, .. }));
    assert_eq!(sim.attempts, 3);
}

#[test]
fn a_vanished_target_after_a_conflict_is_a_lost_race() {
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let out = retry_conflicts(
        "test",
        &mut sim,
        |_| Ok(()),
        |s| {
            s.attempts += 1;
            if s.attempts == 1 {
                Err(conflict())
            } else {
                Err(WalletError::VaultNotFound(OutPoint::default()))
            }
        },
    )
    .unwrap();
    assert_eq!(out, None, "resolved elsewhere, not an error");

    // On the FIRST attempt the same error is a real failure: nothing raced us yet.
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let err = retry_conflicts(
        "test",
        &mut sim,
        |_| Ok(()),
        |_| Err(WalletError::VaultNotFound(OutPoint::default())),
    )
    .unwrap_err();
    assert!(matches!(err, KeeperError::Wallet(WalletError::VaultNotFound(_))));
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
    )
    .unwrap_err();
    assert!(matches!(err, KeeperError::Rejected { .. }));
    assert_eq!(sim.attempts, 1, "no retry on a builder-invariant break");
}

#[test]
fn an_advanced_ratchet_after_a_conflict_is_a_lost_race() {
    use styx_core::units::BlockHeight;
    use styx_pset::error::BuildError;

    let stale = || {
        WalletError::Build(BuildError::RatchetNotAdvanced {
            tick: BlockHeight::new(10),
            last_height: BlockHeight::new(10),
        })
    };

    // Refresh loses the race: the rebuild sees the winner's advanced ratchet.
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let out = retry_conflicts(
        "test",
        &mut sim,
        |_| Ok(()),
        |s| {
            s.attempts += 1;
            if s.attempts == 1 {
                Err(conflict())
            } else {
                Err(stale())
            }
        },
    )
    .unwrap();
    assert_eq!(out, None, "same lost race as a vanished vault");

    // On the first attempt the same error is a real failure (a bug in our tick handling).
    let mut sim = Sim { attempts: 0, resyncs: 0 };
    let err = retry_conflicts("test", &mut sim, |_| Ok(()), |_| Err(stale())).unwrap_err();
    assert!(matches!(err, KeeperError::Wallet(WalletError::Build(_))));
}
