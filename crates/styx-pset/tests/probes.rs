//! The remaining attack probes, and the map of the full inventory to the covenant gates it
//! exercises and the audit findings it re-verifies.
//!
//! | test (file) | covenant gate | finding |
//! |---|---|---|
//! | open_rejects_short/misrouted_borrow_fee (prune_open) | issuer.simf:257,292 | E-2 |
//! | open_rejects_zero_principal (prune_open) | issuer.simf:263 | drain class |
//! | poke_rejects_stale_tick (prune_poke) | issuer.simf:381 | finding A |
//! | draw_rejects_stale_tick (prune_owner_ops) | vault.simf:298 | finding A |
//! | repay_rejects_collateral_skim (prune_owner_ops) | vault.simf repay arm | stealth withdrawal |
//! | refresh_as_draw_pot_drain_rejected (prune_owner_ops) | vault.simf refresh arm | M-2 |
//! | draw_rejects_zero_amount (this file) | issuer.simf:355 | Hole A |
//! | draw_rejects_decoy_pot (this file) | issuer.simf:343-346 | Hole B |
//! | open_rejects_non_obol_pot (this file) | issuer.simf open arm pins | asset confusion |
//! | refresh_rejects_unhealthy (prune_owner_ops) | vault.simf:458 | M-2 |
//! | liquidate_rejects_stale_tick (prune_liquidations) | vault.simf:321 | finding A |
//! | liquidate_rejects_zero_dd / full_repayment | vault.simf:324,327 | strict partial |
//! | liquidate under/over-heal, extraction cap | vault.simf:346-353 | E-1 |
//! | liquidate short fee, sybil reserve | vault.simf:357 + STABILITY_SPK pin | fee griefing |
//! | full_liq band gates (prune_liquidations) | vault.simf:377-379,402 | E-3 |
//! | bad_debt_rejects_stale_tick_vs_issuer_anchor | issuer.simf attest floor | finding A |
//! | bad_debt_rejects_fake_vault_at_input_0 | issuer.simf attest reconstruction | finding B |
//! | bad_debt_rejects_without_issuer_cospend | stability.simf bad-debt token gate | finding B |
//! | bad_debt_rejects_reserve_at_wrong_index | attest/stability index pins | finding B |
//! | attest_rejects_recap_bypass | issuer.simf attest reconstruction | finding B |
//! | bad_debt_reserve_pay_capped_at_20_percent | issuer.simf:471 | M-1 |
//! | redeem_rejects_stale_tick (prune_redeem) | vault.simf:415 | finding A |
//! | redeem_rejects_x_above_debt (prune_redeem) | vault.simf:419-420 | - |
//! | redeem_rejects_par_extraction_when_underbacked | vault.simf:438-439 | E-2 tail |
//! | poke_rejected_at_nonzero_index (this file) | issuer.simf:372 | pot-drain guard |
//! | vault_rejected_at_nonzero_index (this file) | vault.simf:258 | finding 2 |
//! | open_rejects_zero_price (this file) | vault.simf:84 twin in issuer | zero-price guard |

use styx_core::units::Obol;
use styx_pset::build::draw::draw_unchecked;
use styx_pset::build::open::{open, open_unchecked};
use styx_pset::error::BuildError;
use styx_pset::intent::DrawIntent;
use styx_pset::layout::claimed;
use styx_pset::testkit::scenarios::{self};
use styx_pset::testkit::{
    assert_rejects, op_true_spk, open_intent, poke_intent, protocol_state, slot_verdict, TestDeploy, FEE,
};

#[test]
fn poke_rejected_at_nonzero_index() {
    // POKE is pinned to input 0 so it can never co-spend a pot outflow or a reserve shrink
    // (which need the token at an input >= 1). Swap the issuer to input 1.
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 0, 100);
    let built = styx_pset::build::poke::poke(&d.ctx, &state.issuer, &poke_intent(d.tick(120, 120_000)))
        .expect("builds");
    let mut plan = built.plan.tamper(|tx, utxos| {
        tx.input.swap(0, 1);
        utxos.swap(0, 1);
    });
    for slot in &mut plan.slots {
        if slot.input == 0 {
            slot.input = 1;
        }
    }
    assert_rejects(d, &plan);
}

#[test]
fn open_rejects_zero_price() {
    // A zero-price tick is signable (three honest signatures over price 0) but the quorum
    // layer rejects it: coll_at_cr's divide-by-zero-yields-0 must never price a mint.
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let intent = open_intent(d.tick(120, 0), Obol::new(5_000_000));
    assert!(matches!(open(&d.ctx, &state, &intent), Err(BuildError::ZeroPrice)));
    let built = open_unchecked(&d.ctx, &state, &intent, styx_core::units::Sats::ZERO);
    assert!(!slot_verdict(d, &built.plan, 1), "the issuer quorum must reject a zero price");
}

#[test]
fn draw_rejects_zero_amount() {
    // Hole A: a d = 0 draw would advance state while releasing nothing - and its dual, a
    // zero-release layout, would let the pot be nibbled. Both covenant sides assert 0 < d.
    let d = TestDeploy::get();
    let s = scenarios::draw(d);
    let vault = styx_core::domain::OnChain {
        state: s.vault,
        outpoint: s.plan.tx.input[0].previous_output,
        value: styx_core::units::Sats::new(62_500_000),
    };
    let protocol = protocol_state(95_000_000, 0, 100);
    let intent = DrawIntent {
        amount: Obol::ZERO,
        borrower_spk: op_true_spk(),
        tick: d.tick(120, 120_000),
        fee: FEE,
    };
    let built = draw_unchecked(&d.ctx, &protocol, &vault, &intent);
    assert!(!slot_verdict(d, &built.plan, 2), "the issuer draw arm must reject d = 0");
}

#[test]
fn draw_rejects_decoy_pot() {
    // Hole B: the attacker supplies their own OBOL coin as the "pot" at input 1 (no outflow
    // witness needed on an op_true coin). The issuer pins the genuine pot by POT_SPK.
    let d = TestDeploy::get();
    let s = scenarios::draw(d);
    let mut plan = s.plan.tamper(|_, utxos| {
        utxos[1] = claimed(95_000_000, op_true_spk(), d.ctx.params.obol);
    });
    plan.slots.retain(|slot| slot.input != 1);
    assert!(!slot_verdict(d, &plan, 2), "the issuer POT_SPK pin must reject a decoy pot");
}

#[test]
fn open_rejects_non_obol_pot() {
    // Asset confusion: the claimed pot UTXO carries the policy asset instead of OBOL. The
    // issuer's explicit reads pin the pot's asset, so a same-script wrong-asset coin fails.
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let built =
        open(&d.ctx, &state, &open_intent(d.tick(120, 120_000), Obol::new(5_000_000))).expect("builds");
    let plan = built.plan.tamper(|_, utxos| {
        utxos[0] = claimed(100_000_000, d.ctx.artifacts.pot_spk(), d.ctx.params.policy);
    });
    assert!(!slot_verdict(d, &plan, 1), "the issuer must reject a non-OBOL pot");
}

#[test]
fn vault_rejected_at_nonzero_index() {
    // Finding 2: every permissionless vault arm pins the vault to input 0, so a second vault
    // cannot ride along at another index and alias the checks. Swap the vault to input 2.
    let d = TestDeploy::get();
    let s = scenarios::liquidate(d);
    let mut plan = s.plan.tamper(|tx, utxos| {
        tx.input.swap(0, 2);
        utxos.swap(0, 2);
    });
    for slot in &mut plan.slots {
        if slot.input == 0 {
            slot.input = 2;
        }
    }
    assert_rejects(d, &plan);
}
