//! REDEEM at the prune tier: the builder, the backing floor (E-2 tail), and the boundaries.

use styx_core::domain::OnChain;
use styx_core::elements::confidential::Value as CValue;
use styx_core::units::{Obol, Sats};
use styx_pset::build::redeem::redeem;
use styx_pset::error::BuildError;
use styx_pset::intent::RedeemIntent;
use styx_pset::testkit::scenarios;
use styx_pset::testkit::{
    assert_accepts, assert_rejects, op_true_spk, protocol_state, synthetic_outpoint, TestDeploy, FEE,
};

fn intent(d: &TestDeploy, x: u64, backing_k: styx_core::units::RatioK) -> RedeemIntent {
    RedeemIntent {
        x: Obol::new(x),
        redeemer: styx_pset::intent::ObolCoin {
            outpoint: synthetic_outpoint(0xB4),
            value: Obol::new(x),
            spk: op_true_spk(),
        },
        redeemer_spk: op_true_spk(),
        obol_change_spk: op_true_spk(),
        tick: d.tick_bk(120, 120_000, backing_k),
        fee: FEE,
    }
}

fn vault(d: &TestDeploy) -> OnChain<styx_core::domain::VaultState> {
    let s = scenarios::redeem(d);
    OnChain { state: s.vault, outpoint: synthetic_outpoint(0xC0), value: Sats::new(62_500_000) }
}

const PAR: styx_core::units::RatioK = styx_core::consts::K_PAR;

#[test]
fn redeem_accepts_at_par() {
    let d = TestDeploy::get();
    let s = scenarios::redeem(d);
    assert_accepts(d, &s.plan);
}

#[test]
fn redeem_accepts_the_full_debt() {
    // x == debt is the covenant's boundary (x in (0, debt]): a full redemption force-closes
    // the position to a zero-debt vault (E-8 residual).
    let d = TestDeploy::get();
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let built = redeem(&d.ctx, &protocol, &vault(d), &intent(d, 5_000_000, PAR)).expect("builds");
    assert_accepts(d, &built.plan);
    assert_eq!(built.expected.vault.state.debt, Obol::ZERO);
}

#[test]
fn redeem_rejects_par_extraction_when_underbacked() {
    // The E-2 tail: under an 80%-backed tick the covenant caps the extraction at the
    // pro-rata share. Resize the payout to the par value (one pair of outputs, conservation
    // kept): the backing floor must reject.
    let d = TestDeploy::get();
    let s = scenarios::redeem_underbacked(d);
    assert_accepts(d, &s.plan); // the pro-rata twin
    let par_minus_floor = 8_333_333u64 - 6_666_666; // par worth - 80% worth of $10k at $120k
    let plan = s.plan.tamper(|tx, _| {
        let CValue::Explicit(v0) = tx.output[0].value else { panic!("explicit") };
        let CValue::Explicit(v2) = tx.output[2].value else { panic!("explicit") };
        tx.output[0].value = CValue::Explicit(v0 - par_minus_floor);
        tx.output[2].value = CValue::Explicit(v2 + par_minus_floor);
    });
    let rejected = assert_rejects(d, &plan);
    assert_eq!(rejected.input, 0, "the vault's backing floor is the rejector");
}

#[test]
fn redeem_rejects_stale_tick() {
    // The strict vault ratchet for collateral-removing ops (vault.simf:415): a tick at
    // exactly last_height is stale. Both tiers.
    let d = TestDeploy::get();
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let mut i = intent(d, 1_000_000, PAR);
    i.tick = d.tick(100, 120_000);
    assert!(matches!(
        redeem(&d.ctx, &protocol, &vault(d), &i),
        Err(BuildError::RatchetNotAdvanced { .. })
    ));
    let built = styx_pset::build::redeem::redeem_unchecked(&d.ctx, &protocol, &vault(d), &i);
    let rejected = assert_rejects(d, &built.plan);
    assert_eq!(rejected.input, 0);
}

#[test]
fn redeem_rejects_x_above_debt() {
    // x <= debt is the covenant's underflow assert (vault.simf:419-420). Both tiers.
    let d = TestDeploy::get();
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let i = intent(d, 5_000_001, PAR);
    assert!(matches!(
        redeem(&d.ctx, &protocol, &vault(d), &i),
        Err(BuildError::AmountExceedsDebt { .. })
    ));
    let built = styx_pset::build::redeem::redeem_unchecked(&d.ctx, &protocol, &vault(d), &i);
    let rejected = assert_rejects(d, &built.plan);
    assert_eq!(rejected.input, 0);
}

#[test]
fn redeem_builder_refuses_zero_x() {
    let d = TestDeploy::get();
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    assert!(matches!(
        redeem(&d.ctx, &protocol, &vault(d), &intent(d, 0, PAR)),
        Err(BuildError::ZeroAmount)
    ));
}
