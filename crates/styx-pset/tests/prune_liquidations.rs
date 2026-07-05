//! The liquidation family at the prune tier: partial / full-liq / bad-debt builders, their
//! gates, and the ported audit probes. Each negative is single-cause: the untampered twin is
//! an accepting scenario, and exactly one thing differs.

use styx_core::domain::OnChain;
use styx_core::elements::confidential::Value as CValue;
use styx_core::encode::IssuerOp;
use styx_core::params::xonly_u256;
use styx_core::units::{Obol, Sats};
use styx_pset::build::{bad_debt::bad_debt_unchecked, full_liq::full_liq, liquidate::liquidate};
use styx_pset::error::BuildError;
use styx_pset::plan::SlotKind;
use styx_pset::testkit::scenarios::{self, bad_debt_intent, liquidate_intent, set_slot_kind};
use styx_pset::testkit::{
    assert_accepts, assert_rejects, op_true_spk, protocol_state, slot_verdict, TestDeploy,
};

// --- partial liquidate -----------------------------------------------------------

#[test]
fn liquidate_heals_into_band() {
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    assert_accepts(d, &s.plan);
}

#[test]
fn liquidate_rejects_stale_tick() {
    // A tick at exactly last_height: the vault ratchet is strict for collateral-removing
    // ops. Builder refusal plus the covenant verdict on the otherwise-consistent tx.
    let d = TestDeploy::get();
    let vault = plan_vault(&scenarios::liquidate(d));
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let intent = liquidate_intent(d.tick(100, 63_000));
    assert!(matches!(
        liquidate(&d.ctx, &protocol, &vault, &intent),
        Err(BuildError::RatchetNotAdvanced { .. })
    ));
    let built = styx_pset::build::liquidate::liquidate_unchecked(&d.ctx, &protocol, &vault, &intent);
    assert_rejects(d, &built.plan);
}

#[test]
fn liquidate_builder_refuses_healthy_vault() {
    // At $120k the base vault sits at 150%: partial liquidation is closed.
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    let vault = OnChain { value: Sats::new(100_000_000), ..plan_vault(&s) };
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    assert!(matches!(
        liquidate(&d.ctx, &protocol, &vault, &liquidate_intent(d.tick(120, 120_000))),
        Err(BuildError::VaultTooHealthy { .. })
    ));
}

fn plan_vault(s: &scenarios::Scenario) -> OnChain<styx_core::domain::VaultState> {
    OnChain {
        state: s.vault,
        outpoint: s.plan.tx.input[0].previous_output,
        value: Sats::new(100_000_000),
    }
}

/// The heal band edges for the canonical scenario's residual debt at the given max quote.
fn heal_band(rd_cents: u32, price: u32) -> (Sats, Sats) {
    use styx_core::consts::{K_HEAL_HI, K_HEAL_LO};
    let p = styx_core::units::Price::new(price);
    (
        styx_core::math::coll_at_cr(rd_cents, p, K_HEAL_LO),
        styx_core::math::coll_at_cr(rd_cents, p, K_HEAL_HI),
    )
}

#[test]
fn liquidate_rejects_under_heal() {
    // Residual one sat below the 132% band floor: builder refusal + covenant rejection.
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    let vault = plan_vault(&s);
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let mut intent = liquidate_intent(d.tick(120, 63_000));
    let (lo, _) = heal_band(3_500_000, 63_000);
    intent.residual = Sats::new(lo.raw() - 1);
    assert!(matches!(
        liquidate(&d.ctx, &protocol, &vault, &intent),
        Err(BuildError::HealOutOfBand { .. })
    ));
    let built = styx_pset::build::liquidate::liquidate_unchecked(&d.ctx, &protocol, &vault, &intent);
    assert_rejects(d, &built.plan);
}

#[test]
fn liquidate_rejects_over_heal() {
    // Residual one sat above the 137% ceiling: over-liquidation of the healthy remainder.
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    let vault = plan_vault(&s);
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let mut intent = liquidate_intent(d.tick(120, 63_000));
    let (_, hi) = heal_band(3_500_000, 63_000);
    intent.residual = Sats::new(hi.raw() + 1);
    assert!(matches!(
        liquidate(&d.ctx, &protocol, &vault, &intent),
        Err(BuildError::HealOutOfBand { .. })
    ));
    let built = styx_pset::build::liquidate::liquidate_unchecked(&d.ctx, &protocol, &vault, &intent);
    assert_rejects(d, &built.plan);
}

#[test]
fn liquidate_rejects_full_repayment() {
    // dd == debt is full-liq's shape in the partial arm: the heal band collapses to [0, 0]
    // and the covenant requires a positive residual debt (vault.simf:327). Both tiers.
    let d = TestDeploy::get();
    let vault = plan_vault(&scenarios::liquidate(d));
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let mut intent = liquidate_intent(d.tick(120, 63_000));
    intent.dd = vault.state.debt;
    intent.residual = Sats::ZERO;
    intent.keeper.value = vault.state.debt;
    assert!(matches!(liquidate(&d.ctx, &protocol, &vault, &intent), Err(BuildError::NotPartial { .. })));
    let built = styx_pset::build::liquidate::liquidate_unchecked(&d.ctx, &protocol, &vault, &intent);
    let rejected = assert_rejects(d, &built.plan);
    assert_eq!(rejected.input, 0, "the vault's partial arm is the rejector");
}

#[test]
fn liquidate_rejects_zero_dd() {
    // 0 < dd is the covenant's own gate (vault.simf:324); the builder refuses explicitly.
    let d = TestDeploy::get();
    let vault = plan_vault(&scenarios::liquidate(d));
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let mut intent = liquidate_intent(d.tick(120, 63_000));
    intent.dd = Obol::ZERO;
    assert!(matches!(liquidate(&d.ctx, &protocol, &vault, &intent), Err(BuildError::ZeroAmount)));
    let built = styx_pset::build::liquidate::liquidate_unchecked(&d.ctx, &protocol, &vault, &intent);
    let rejected = assert_rejects(d, &built.plan);
    assert_eq!(rejected.input, 0);
}

#[test]
fn liquidate_rejects_extraction_over_cap() {
    // The partial's core economic bound: the owner loses at most 1.15 x dd. Collateral 102M
    // is still under the 130% gate (~103.17M), but the band-floor residual leaves an
    // extraction above the cap - builder refusal + covenant rejection at the vault slot.
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    let vault = OnChain { value: Sats::new(102_000_000), ..plan_vault(&s) };
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let mut intent = liquidate_intent(d.tick(120, 63_000));
    let (lo, _) = heal_band(3_500_000, 63_000);
    intent.residual = lo;
    assert!(matches!(
        liquidate(&d.ctx, &protocol, &vault, &intent),
        Err(BuildError::ExtractionExceedsCap { .. })
    ));
    let built = styx_pset::build::liquidate::liquidate_unchecked(&d.ctx, &protocol, &vault, &intent);
    let rejected = assert_rejects(d, &built.plan);
    assert_eq!(rejected.input, 0, "the vault's extraction cap is the rejector");
}

#[test]
fn liquidate_rejects_short_stability_fee() {
    // One sat short on the reserve's 5% share: the reserve's grow-only arm still passes, the
    // vault's exact share pin rejects.
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    let plan = s.plan.tamper(|tx, _| {
        let CValue::Explicit(v) = tx.output[3].value else { panic!("explicit") };
        tx.output[3].value = CValue::Explicit(v - 1);
    });
    assert!(slot_verdict(d, &plan, 3), "reserve grow-only arm must still accept");
    assert_rejects(d, &plan);
}

#[test]
fn liquidate_rejects_sybil_reserve() {
    // The full share amount routed to a non-reserve script: the constant-address pin closes
    // the fee-griefing sybil (the reserve is one scriptHash, not a per-keeper address).
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    let plan = s.plan.tamper(|tx, _| {
        tx.output[3].script_pubkey = op_true_spk();
    });
    assert_rejects(d, &plan);
}

// --- full liquidation --------------------------------------------------------------

#[test]
fn full_liq_accepts_in_band() {
    let d = TestDeploy::get();
    let s = scenarios::full_liq(d);
    assert_accepts(d, &s.plan);
}

#[test]
fn full_liq_rejects_above_the_band() {
    // At $95k the vault sits at ~118% CR: partial territory, full-liq is closed.
    let d = TestDeploy::get();
    let s = scenarios::full_liq(d);
    let vault = OnChain {
        state: s.vault,
        outpoint: s.plan.tx.input[0].previous_output,
        value: Sats::new(62_500_000),
    };
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let fl = styx_pset::intent::FullLiqIntent {
        keeper: styx_pset::intent::ObolCoin {
            outpoint: s.plan.tx.input[2].previous_output,
            value: Obol::new(5_001_000),
            spk: op_true_spk(),
        },
        keeper_spk: op_true_spk(),
        obol_change_spk: op_true_spk(),
        tick: d.tick(120, 95_000),
        fee: styx_pset::testkit::FEE,
    };
    assert!(matches!(full_liq(&d.ctx, &protocol, &vault, &fl), Err(BuildError::CrOutOfBand { .. })));
    let built = styx_pset::build::full_liq::full_liq_unchecked(&d.ctx, &protocol, &vault, &fl);
    assert_rejects(d, &built.plan);
}

#[test]
fn full_liq_rejects_below_the_band() {
    // At $75k the vault is underwater (~94% CR): bad-debt territory, not full-liq.
    let d = TestDeploy::get();
    let s = scenarios::full_liq(d);
    let vault = OnChain {
        state: s.vault,
        outpoint: s.plan.tx.input[0].previous_output,
        value: Sats::new(62_500_000),
    };
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let fl = styx_pset::intent::FullLiqIntent {
        keeper: styx_pset::intent::ObolCoin {
            outpoint: s.plan.tx.input[2].previous_output,
            value: Obol::new(5_001_000),
            spk: op_true_spk(),
        },
        keeper_spk: op_true_spk(),
        obol_change_spk: op_true_spk(),
        tick: d.tick(120, 75_000),
        fee: styx_pset::testkit::FEE,
    };
    assert!(matches!(full_liq(&d.ctx, &protocol, &vault, &fl), Err(BuildError::CrOutOfBand { .. })));
    let built = styx_pset::build::full_liq::full_liq_unchecked(&d.ctx, &protocol, &vault, &fl);
    assert_rejects(d, &built.plan);
}

// --- bad debt ------------------------------------------------------------------------

#[test]
fn bad_debt_accepts_with_reserve_cover() {
    let d = TestDeploy::get();
    let s = scenarios::bad_debt(d);
    assert_accepts(d, &s.plan);
}

#[test]
fn bad_debt_drains_a_poor_reserve_gracefully() {
    // E-2 partial reserve: a 10M reserve is below the 25M cap, so the min(..., balance)
    // branch binds and the reserve drains to zero rather than blocking the close.
    let d = TestDeploy::get();
    let s = scenarios::bad_debt(d);
    let vault = OnChain {
        state: s.vault,
        outpoint: s.plan.tx.input[0].previous_output,
        value: Sats::new(62_500_000),
    };
    let protocol = protocol_state(95_000_000, 10_000_000, 100);
    let built = styx_pset::build::bad_debt::bad_debt(
        &d.ctx,
        &protocol,
        &vault,
        &bad_debt_intent(d.tick(120, 40_000)),
    )
    .expect("builds");
    assert_accepts(d, &built.plan);
    assert_eq!(built.expected.reserve.value, Sats::ZERO);
}

#[test]
fn bad_debt_rejects_stale_tick_vs_issuer_anchor() {
    // The issuer anchor sits above the tick: the attest floor rejects even though the vault
    // ratchet (lh 100 < 120) passes.
    let d = TestDeploy::get();
    let vault = OnChain {
        state: scenarios::bad_debt(d).vault,
        outpoint: styx_pset::testkit::synthetic_outpoint(0xC0),
        value: Sats::new(62_500_000),
    };
    let protocol = protocol_state(95_000_000, 30_000_000, 130);
    let intent = bad_debt_intent(d.tick(120, 40_000));
    assert!(matches!(
        styx_pset::build::bad_debt::bad_debt(&d.ctx, &protocol, &vault, &intent),
        Err(BuildError::StaleTick { .. })
    ));
    let built = bad_debt_unchecked(&d.ctx, &protocol, &vault, &intent);
    assert_rejects(d, &built.plan);
}

#[test]
fn bad_debt_rejects_fake_vault_at_input_0() {
    // Finding B: the keeper claims their own op_true coin as the "underwater vault". The
    // attest arm reconstructs the vault spk from (debt, owner, last_height) and pins input 0
    // to it; a non-vault script cannot match.
    let d = TestDeploy::get();
    let s = scenarios::bad_debt(d);
    let mut plan = s.plan.tamper(|_, utxos| {
        utxos[0] = styx_pset::layout::claimed(62_500_000, op_true_spk(), d.ctx.params.policy);
    });
    plan.slots.retain(|slot| slot.input != 0); // the attacker provides no vault witness
    assert!(!slot_verdict(d, &plan, 4), "attest must reject a non-vault input 0");
}

#[test]
fn bad_debt_rejects_without_issuer_cospend() {
    // The reserve's bad-debt arm gates on the issuer token at input 4; a policy coin there
    // (and no attest witness) leaves the reserve shrink unauthorized.
    let d = TestDeploy::get();
    let s = scenarios::bad_debt(d);
    let mut plan = s.plan.tamper(|_, utxos| {
        utxos[4] = styx_pset::layout::claimed(1, op_true_spk(), d.ctx.params.policy);
    });
    plan.slots.retain(|slot| slot.input != 4);
    assert!(!slot_verdict(d, &plan, 3), "the reserve must reject without the issuer token");
}

#[test]
fn attest_rejects_recap_bypass() {
    // The attest op understates the vault's debt: the reconstructed vault spk no longer
    // matches input 0, so a smaller pot repayment cannot be attested for a bigger vault.
    let d = TestDeploy::get();
    let s = scenarios::bad_debt(d);
    let mut plan = s.plan.clone();
    let state = match &plan.slots.iter().find(|sl| sl.input == 4).expect("issuer slot").kind {
        SlotKind::Issuer { state, .. } => *state,
        _ => panic!("not an issuer slot"),
    };
    set_slot_kind(
        &mut plan,
        4,
        SlotKind::Issuer {
            state,
            op: Box::new(IssuerOp::Attest {
                debt: Obol::new(s.vault.debt.raw() - 1),
                owner: xonly_u256(&s.vault.owner),
                last_height: s.vault.last_height,
                tick: s.tick.clone(),
            }),
        },
    );
    assert!(!slot_verdict(d, &plan, 4), "attest must reject an understated debt");
}

#[test]
fn bad_debt_rejects_reserve_at_wrong_index() {
    // The reserve spent at input 5 instead of 3 (swapped with the fee coin): the covenants
    // pin the bad-debt layout by index, so the displaced reserve is rejected.
    let d = TestDeploy::get();
    let s = scenarios::bad_debt(d);
    let mut plan = s.plan.tamper(|tx, utxos| {
        tx.input.swap(3, 5);
        utxos.swap(3, 5);
    });
    for slot in &mut plan.slots {
        if slot.input == 3 {
            slot.input = 5;
        }
    }
    assert_rejects(d, &plan);
}

#[test]
fn bad_debt_reserve_pay_capped_at_20_percent() {
    // M-1: the scenario's cap binds (25M of a 30M reserve). One sat more to the keeper, one
    // less to the reserve remainder - the pay amount is the single cause.
    let d = TestDeploy::get();
    let s = scenarios::bad_debt(d);
    let plan = s.plan.tamper(|tx, _| {
        let CValue::Explicit(k) = tx.output[0].value else { panic!("explicit") };
        let CValue::Explicit(r) = tx.output[2].value else { panic!("explicit") };
        tx.output[0].value = CValue::Explicit(k + 1);
        tx.output[2].value = CValue::Explicit(r - 1);
    });
    assert_rejects(d, &plan);
}
