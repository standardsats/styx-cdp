//! Owner ops at the prune tier: CLOSE / REPAY / DRAW / REFRESH builders, their gates, and
//! the ported audit probes (collateral skim, stale tick, unhealthy refresh, refresh-as-draw,
//! wrong owner key).

use styx_core::domain::{OnChain, VaultState};
use styx_core::elements::confidential::Value as CValue;
use styx_core::encode::VaultOp;
use styx_core::units::{BlockHeight, Obol, Sats};
use styx_pset::build::{close::close, draw::draw, refresh::refresh, repay::repay};
use styx_pset::error::BuildError;
use styx_pset::intent::{CloseIntent, DrawIntent, FundingCoin, ObolCoin, RefreshIntent, RepayIntent};
use styx_pset::sign::owner_sign;
use styx_pset::testkit::scenarios::{self, set_vault_op};
use styx_pset::testkit::{
    assert_accepts, assert_rejects, keypair, op_true_spk, protocol_state, synthetic_outpoint,
    TestDeploy, FEE,
};

fn vault_on_chain(debt: u64, coll: u64) -> OnChain<VaultState> {
    OnChain {
        state: VaultState {
            debt: Obol::new(debt),
            owner: keypair(10).x_only_public_key().0,
            last_height: BlockHeight::new(100),
        },
        outpoint: synthetic_outpoint(0xC0),
        value: Sats::new(coll),
    }
}

fn obol_coin(value: u64) -> ObolCoin {
    ObolCoin { outpoint: synthetic_outpoint(0xB2), value: Obol::new(value), spk: op_true_spk() }
}

fn fee_coin() -> FundingCoin {
    FundingCoin { outpoint: synthetic_outpoint(0xB3), value: Sats::new(1_000_000), spk: op_true_spk() }
}

#[test]
fn close_accepts_full_debt_burn() {
    let d = TestDeploy::get();
    let pot = protocol_state(95_000_000, 0, 100).pot;
    let vault = vault_on_chain(5_000_000, 62_500_000);
    // The payer overpays; the surplus returns as OBOL change.
    let intent = CloseIntent {
        payer: obol_coin(6_000_000),
        recipient_spk: op_true_spk(),
        payer_change_spk: op_true_spk(),
        fee: FEE,
    };
    let mut built = close(&d.ctx, &pot, &vault, &intent).expect("builds");
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_accepts(d, &built.plan);
    assert_eq!(built.expected.pot.value, Obol::new(100_000_000));
}

#[test]
fn close_rejects_underpayment() {
    // One OBOL unit short on the pot successor: the close arm pins pot + full debt.
    let d = TestDeploy::get();
    let pot = protocol_state(95_000_000, 0, 100).pot;
    let vault = vault_on_chain(5_000_000, 62_500_000);
    let intent = CloseIntent {
        payer: obol_coin(5_000_000),
        recipient_spk: op_true_spk(),
        payer_change_spk: op_true_spk(),
        fee: FEE,
    };
    let mut built = close(&d.ctx, &pot, &vault, &intent).expect("builds");
    built.plan = built.plan.tamper(|tx, _| {
        let CValue::Explicit(v) = tx.output[1].value else { panic!("explicit") };
        tx.output[1].value = CValue::Explicit(v - 1);
    });
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_rejects(d, &built.plan);
}

#[test]
fn repay_accepts_and_preserves_collateral() {
    let d = TestDeploy::get();
    let pot = protocol_state(95_000_000, 0, 100).pot;
    let vault = vault_on_chain(5_000_000, 62_500_000);
    let intent = RepayIntent {
        amount: Obol::new(2_000_000),
        payer: obol_coin(2_000_000),
        payer_change_spk: op_true_spk(),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        fee: FEE,
    };
    let mut built = repay(&d.ctx, &pot, &vault, &intent).expect("builds");
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_accepts(d, &built.plan);
    assert_eq!(built.expected.vault.state.debt, Obol::new(3_000_000));
    assert_eq!(built.expected.vault.value, vault.value);
}

#[test]
fn repay_rejects_collateral_skim() {
    // One sat off the preserved collateral: the covenant rejects ANY reduction, so a stealth
    // withdrawal through REPAY is impossible.
    let d = TestDeploy::get();
    let pot = protocol_state(95_000_000, 0, 100).pot;
    let vault = vault_on_chain(5_000_000, 62_500_000);
    let intent = RepayIntent {
        amount: Obol::new(2_000_000),
        payer: obol_coin(2_000_000),
        payer_change_spk: op_true_spk(),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        fee: FEE,
    };
    let mut built = repay(&d.ctx, &pot, &vault, &intent).expect("builds");
    built.plan = built.plan.tamper(|tx, _| {
        let CValue::Explicit(v) = tx.output[0].value else { panic!("explicit") };
        tx.output[0].value = CValue::Explicit(v - 1);
    });
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_rejects(d, &built.plan);
}

#[test]
fn repay_accepts_full_debt() {
    // r == debt: the covenant's only boundary is the underflow assert, so a full repayment
    // through REPAY is legal and leaves a zero-debt vault on chain - a state the M11 scanner
    // must read. CLOSE differs only in freeing the collateral.
    let d = TestDeploy::get();
    let pot = protocol_state(95_000_000, 0, 100).pot;
    let vault = vault_on_chain(5_000_000, 62_500_000);
    let intent = RepayIntent {
        amount: Obol::new(5_000_000),
        payer: obol_coin(5_000_000),
        payer_change_spk: op_true_spk(),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        fee: FEE,
    };
    let mut built = repay(&d.ctx, &pot, &vault, &intent).expect("builds");
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_accepts(d, &built.plan);
    assert_eq!(built.expected.vault.state.debt, Obol::ZERO);
}

#[test]
fn draw_accepts_at_exactly_150() {
    // The covenant's post-draw gate is non-strict (le_64): post-fee collateral exactly at
    // 150% of the new debt accepts; one sat under rejects at both tiers. OPEN's twin of this
    // boundary is pinned in M5; this is DRAW's.
    let d = TestDeploy::get();
    let state = protocol_state(95_000_000, 0, 100);
    let need = 50_000_000u64; // coll_at_cr(4M cents, $120k, 150%)
    let vault = vault_on_chain(3_000_000, need + FEE.raw());
    let intent = DrawIntent {
        amount: Obol::new(1_000_000),
        borrower_spk: op_true_spk(),
        tick: d.tick(120, 120_000),
        fee: FEE,
    };
    let mut built = draw(&d.ctx, &state, &vault, &intent).expect("builds at the boundary");
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_accepts(d, &built.plan);

    let short = OnChain { value: Sats::new(need + FEE.raw() - 1), ..vault };
    assert!(matches!(
        draw(&d.ctx, &state, &short, &intent),
        Err(BuildError::Undercollateralized { .. })
    ));
    let mut built = styx_pset::build::draw::draw_unchecked(&d.ctx, &state, &short, &intent);
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_rejects(d, &built.plan);
}

#[test]
fn draw_accepts_at_150() {
    let d = TestDeploy::get();
    let state = protocol_state(95_000_000, 0, 100);
    let vault = vault_on_chain(3_000_000, 62_500_000);
    let intent = DrawIntent {
        amount: Obol::new(1_000_000),
        borrower_spk: op_true_spk(),
        tick: d.tick(120, 120_000),
        fee: FEE,
    };
    let mut built = draw(&d.ctx, &state, &vault, &intent).expect("builds");
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_accepts(d, &built.plan);
    assert_eq!(built.expected.vault.state.debt, Obol::new(4_000_000));
    assert_eq!(built.expected.vault.state.last_height, BlockHeight::new(120));
}

#[test]
fn draw_rejects_stale_tick() {
    // The vault ratchet is strict: a tick at exactly last_height is stale for DRAW (the
    // issuer's global floor is non-strict, so the vault is the sole rejector here).
    let d = TestDeploy::get();
    let state = protocol_state(95_000_000, 0, 100);
    let vault = vault_on_chain(3_000_000, 62_500_000);
    let intent = DrawIntent {
        amount: Obol::new(1_000_000),
        borrower_spk: op_true_spk(),
        tick: d.tick(100, 120_000),
        fee: FEE,
    };
    assert!(matches!(draw(&d.ctx, &state, &vault, &intent), Err(BuildError::RatchetNotAdvanced { .. })));

    let mut built = styx_pset::build::draw::draw_unchecked(&d.ctx, &state, &vault, &intent);
    owner_sign(&d.ctx, &mut built.plan, &keypair(10)).expect("signs");
    assert_rejects(d, &built.plan);
}

#[test]
fn refresh_accepts_healthy() {
    let d = TestDeploy::get();
    let vault = vault_on_chain(5_000_000, 62_500_000); // CR 150% at $120k >= the 130% gate
    let intent = RefreshIntent {
        tick: d.tick(120, 120_000),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        fee: FEE,
    };
    let built = refresh(&d.ctx, &vault, &intent).expect("builds");
    // Permissionless: no owner signature involved.
    assert_accepts(d, &built.plan);
    assert_eq!(built.expected.vault.state.last_height, BlockHeight::new(120));
}

#[test]
fn refresh_rejects_unhealthy() {
    // At the $63k dip the vault sits at CR ~126% < 130%: refresh must not advance the
    // ratchet of a liquidatable vault (it would shelter it from the dip tick).
    let d = TestDeploy::get();
    let vault = vault_on_chain(5_000_000, 62_500_000);
    let intent = RefreshIntent {
        tick: d.tick(120, 63_000),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        fee: FEE,
    };
    assert!(matches!(refresh(&d.ctx, &vault, &intent), Err(BuildError::Undercollateralized { .. })));
    let built = styx_pset::build::refresh::refresh_unchecked(&d.ctx, &vault, &intent);
    assert_rejects(d, &built.plan);
}

#[test]
fn refresh_as_draw_pot_drain_rejected() {
    // The M-2 probe: a REFRESH encoding on a DRAW-shaped tx (pot released, collateral
    // shrunk) must reject - refresh pins collateral preservation and touches no pot.
    let d = TestDeploy::get();
    let s = scenarios::draw(d);
    let mut plan = s.plan;
    set_vault_op(&mut plan, VaultOp::Refresh { tick: s.tick });
    assert_rejects(d, &plan);
}

#[test]
fn owner_sig_from_wrong_key_rejected() {
    let d = TestDeploy::get();
    let pot = protocol_state(95_000_000, 0, 100).pot;
    let vault = vault_on_chain(5_000_000, 62_500_000);
    let intent = RepayIntent {
        amount: Obol::new(2_000_000),
        payer: obol_coin(2_000_000),
        payer_change_spk: op_true_spk(),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        fee: FEE,
    };
    let mut built = repay(&d.ctx, &pot, &vault, &intent).expect("builds");
    owner_sign(&d.ctx, &mut built.plan, &keypair(77)).expect("signs"); // the keeper's key
    assert_rejects(d, &built.plan);
}
