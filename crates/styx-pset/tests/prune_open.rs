//! OPEN at the prune tier: four inputs, three covenant slots (pot outflow, reserve
//! accumulate, issuer open). Ports the prototype's `open_fee_probe` (E-2 borrow-fee pin).

use styx_core::consts::{K_FEE_HALF_PERCENT, K_OPEN_MIN};
use styx_core::elements::confidential::Value as CValue;
use styx_core::math::coll_at_cr;
use styx_core::units::{BlockHeight, Obol, Price, Sats};
use styx_pset::build::open::{open, open_unchecked};
use styx_pset::error::BuildError;
use styx_pset::testkit::{
    assert_accepts, assert_rejects, op_true_spk, open_intent, protocol_state, slot_verdict,
    tick_diverging, TestDeploy,
};

/// The scenario of the prototype's LP open: $50k principal at $120k/BTC, 150% CR.
fn genuine() -> (&'static TestDeploy, styx_pset::plan::Built<styx_pset::build::open::OpenDelta>) {
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let intent = open_intent(d.tick(120, 120_000), Obol::new(5_000_000));
    let built = open(&d.ctx, &state, &intent).expect("builds");
    (d, built)
}

#[test]
fn open_accepts() {
    let (d, built) = genuine();
    let tx = assert_accepts(d, &built.plan);
    let v = &built.expected.vault;
    assert_eq!(v.state.debt, Obol::new(5_000_000));
    assert_eq!(v.state.last_height, BlockHeight::new(120));
    assert_eq!(v.outpoint.txid, tx.txid());
    assert_eq!(v.outpoint.vout, 0);
    assert_eq!(built.expected.pot.value, Obol::new(100_000_000 - 5_000_000));
    // 0.5% of $50k at $120k/BTC = 208_333 sats to the reserve.
    assert_eq!(built.expected.reserve.value, Sats::new(1_000_000 + 208_333));
    assert_eq!(built.expected.issuer.state.last_mint_height, BlockHeight::new(120));
}

#[test]
fn open_rejects_short_borrow_fee() {
    // One sat short on the reserve successor. The reserve's own grow-only arm still passes
    // (the output grows), so the only rejecting gate is the issuer's exact +fee pin - the
    // prototype's open_fee_probe, asserted per slot.
    let (d, built) = genuine();
    let plan = built.plan.tamper(|tx, _| {
        let CValue::Explicit(v) = tx.output[3].value else { panic!("explicit") };
        tx.output[3].value = CValue::Explicit(v - 1);
    });
    assert!(slot_verdict(d, &plan, 3), "reserve grow-only arm must still accept");
    assert!(!slot_verdict(d, &plan, 1), "issuer +fee pin must reject");
    assert_rejects(d, &plan);
}

#[test]
fn open_rejects_misrouted_borrow_fee() {
    // The full fee amount, paid to a non-reserve script. Both the issuer's STABILITY_SPK pin
    // and the reserve's own successor check reject; the issuer slot is asserted alone to
    // mirror the prototype probe.
    let (d, built) = genuine();
    let plan = built.plan.tamper(|tx, _| {
        tx.output[3].script_pubkey = op_true_spk();
    });
    assert!(!slot_verdict(d, &plan, 1), "issuer reserve-spk pin must reject");
    assert_rejects(d, &plan);
}

#[test]
fn open_builder_refuses_undercollateralized() {
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let mut intent = open_intent(d.tick(120, 120_000), Obol::new(5_000_000));
    intent.collateral = Sats::new(intent.collateral.raw() - 1); // one sat under 150%
    assert!(matches!(
        open(&d.ctx, &state, &intent),
        Err(BuildError::Undercollateralized { .. })
    ));
}

#[test]
fn open_builder_refuses_stale_tick() {
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let intent = open_intent(d.tick(90, 120_000), Obol::new(5_000_000));
    assert!(matches!(open(&d.ctx, &state, &intent), Err(BuildError::StaleTick { .. })));
}

#[test]
fn open_builder_refuses_principal_above_pot() {
    let d = TestDeploy::get();
    let state = protocol_state(1_000_000, 1_000_000, 100);
    let intent = open_intent(d.tick(120, 120_000), Obol::new(5_000_000));
    assert!(matches!(open(&d.ctx, &state, &intent), Err(BuildError::InsufficientPot { .. })));
}

#[test]
fn open_rejects_zero_principal() {
    // The issuer forbids a zero mint (issuer.simf:264): it would open a pot drain through the
    // inflow leaf. Both tiers: the builder refuses, and the covenant rejects the unchecked
    // build (the reserve and pot arms are indifferent to it, so the issuer slot is the cause).
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let intent = open_intent(d.tick(120, 120_000), Obol::ZERO);
    assert!(matches!(open(&d.ctx, &state, &intent), Err(BuildError::ZeroPrincipal)));

    let built = open_unchecked(&d.ctx, &state, &intent, Sats::ZERO);
    assert!(!slot_verdict(d, &built.plan, 1), "issuer zero-principal gate must reject");
    assert_rejects(d, &built.plan);
}

#[test]
fn open_gates_at_the_min_quote() {
    // Diverging quotes: the issuer prices the 150% gate at the MIN (issuer.simf:187).
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let tick = tick_diverging(d, 120, [120_000, 100_000, 110_000]);

    // Collateral computed at the min quote (open_intent's default): accepted.
    let built = open(&d.ctx, &state, &open_intent(tick.clone(), Obol::new(5_000_000))).expect("builds");
    assert_accepts(d, &built.plan);

    // Collateral sufficient at the max quote but short at the min: the builder refuses, and
    // the covenant rejects the unchecked build at the issuer slot.
    let mut intent = open_intent(tick, Obol::new(5_000_000));
    let debt_cents = intent.principal.covenant_cents().unwrap();
    intent.collateral = coll_at_cr(debt_cents, Price::new(120_000), K_OPEN_MIN);
    assert!(matches!(open(&d.ctx, &state, &intent), Err(BuildError::Undercollateralized { .. })));

    let borrow_fee = coll_at_cr(debt_cents, Price::new(100_000), K_FEE_HALF_PERCENT);
    let built = open_unchecked(&d.ctx, &state, &intent, borrow_fee);
    assert!(!slot_verdict(d, &built.plan, 1), "issuer 150%-at-min gate must reject");
    assert_rejects(d, &built.plan);
}

#[test]
fn open_builder_refuses_funding_mismatch() {
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let mut intent = open_intent(d.tick(120, 120_000), Obol::new(5_000_000));
    intent.funding.value = Sats::new(intent.funding.value.raw() + 1);
    assert!(matches!(open(&d.ctx, &state, &intent), Err(BuildError::FundingMismatch { .. })));
}
