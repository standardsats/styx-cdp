//! POKE at the prune tier: the smallest op (two inputs, one covenant slot), proving the
//! build -> finalize pipeline end to end.

use styx_core::units::{BlockHeight, Sats};
use styx_pset::build::poke::{poke, poke_unchecked};
use styx_pset::error::BuildError;
use styx_pset::testkit::{
    assert_accepts, assert_rejects, op_true_spk, poke_intent, protocol_state, TestDeploy, FEE,
};

#[test]
fn poke_accepts() {
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 0, 100);
    let intent = poke_intent(d.tick(120, 120_000));
    let built = poke(&d.ctx, &state.issuer, &intent).expect("builds");
    let tx = assert_accepts(d, &built.plan);
    assert_eq!(built.expected.issuer.state.last_mint_height, BlockHeight::new(120));
    assert_eq!(built.expected.issuer.outpoint.txid, tx.txid());
    assert_eq!(built.expected.issuer.outpoint.vout, 0);
}

#[test]
fn poke_builder_refuses_a_stale_tick() {
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 0, 100);
    let intent = poke_intent(d.tick(90, 120_000));
    assert!(matches!(
        poke(&d.ctx, &state.issuer, &intent),
        Err(BuildError::StaleTick { .. })
    ));
}

#[test]
fn poke_rejects_stale_tick() {
    // The genuine twin accepts; the same construction with a tick below the anchor is a fully
    // consistent tx (valid quorum signatures, matching nLockTime, successor at the tick
    // height) whose only flaw is the issuer's freshness floor.
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 0, 100);
    assert_accepts(d, &poke_unchecked(&d.ctx, &state.issuer, &poke_intent(d.tick(120, 120_000))).plan);
    assert_rejects(d, &poke_unchecked(&d.ctx, &state.issuer, &poke_intent(d.tick(90, 120_000))).plan);
}

#[test]
fn poke_accepts_a_tick_at_the_anchor_height() {
    // The issuer floor is non-strict (issuer.simf: last_mint_height <= height), so a tick at
    // exactly the anchor height passes - several ops may share one fresh oracle height.
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 0, 100);
    let built = poke(&d.ctx, &state.issuer, &poke_intent(d.tick(100, 120_000))).expect("builds");
    assert_accepts(d, &built.plan);
}

#[test]
fn poke_builder_refuses_underfunded_fee() {
    // Strict: funding == fee would leave a zero-value (dust-nonstandard) change output.
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 0, 100);
    let mut intent = poke_intent(d.tick(120, 120_000));
    intent.funding.value = Sats::new(FEE.raw());
    intent.funding.spk = op_true_spk();
    assert!(matches!(
        poke(&d.ctx, &state.issuer, &intent),
        Err(BuildError::InsufficientFunding { .. })
    ));
}
