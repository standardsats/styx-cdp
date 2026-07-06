//! The decision ladder off node: band-edge table tests, and the property that every verdict
//! is accepted by the checked builder it names - the builders refuse anything the covenants
//! would, so this holds decide()/plan_partial() to covenant-exact integer math (the R4
//! extension of the M9 parity tier).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use styx_core::consts::{K_FULL_LIQ_CAP, K_HEALTH_GATE, K_HEAL_HI, K_HEAL_LO, K_PAR};
use styx_core::domain::{OnChain, VaultState};
use styx_core::math::coll_at_cr;
use styx_core::units::{BlockHeight, Obol, Price, Sats};
use styx_keeper::decide::{decide, plan_partial, Action};
use styx_pset::build;
use styx_pset::intent::{
    BadDebtIntent, FullLiqIntent, FundingCoin, LiquidateIntent, ObolCoin, RefreshIntent,
};
use styx_pset::testkit::{
    keypair, op_true_spk, protocol_state, synthetic_outpoint, tick_diverging, TestDeploy, FEE,
};

const LH: u32 = 5;
const TICK_H: u32 = 30;
const REFRESH_LAG: u32 = 16;
const ANCHOR: BlockHeight = BlockHeight::new(LH);

fn vault(debt: u64, coll: u64) -> OnChain<VaultState> {
    OnChain {
        state: VaultState {
            debt: Obol::new(debt),
            owner: keypair(10).x_only_public_key().0,
            last_height: BlockHeight::new(LH),
        },
        outpoint: synthetic_outpoint(0x50),
        value: Sats::new(coll),
    }
}

#[test]
fn the_ladder_matches_the_covenant_bands_exactly() {
    let d = TestDeploy::get();
    let (debt, price) = (4_000_000u64, 50_000u32);
    let tick = d.tick(TICK_H, price);
    let hi = Price::new(price);
    let floor = coll_at_cr(debt as u32, hi, K_PAR).raw();
    let cap = coll_at_cr(debt as u32, hi, K_FULL_LIQ_CAP).raw();
    let gate = coll_at_cr(debt as u32, hi, K_HEALTH_GATE).raw();

    let at = |coll: u64| decide(&vault(debt, coll), &tick, ANCHOR, REFRESH_LAG, FEE);
    assert_eq!(at(floor - 1), Action::BadDebt, "strictly under water");
    assert_eq!(at(floor), Action::FullLiq, "the 100% floor is full-liq territory");
    assert_eq!(at(cap), Action::FullLiq, "the 115% cap included");
    assert!(matches!(at(cap + 1), Action::Partial { .. }), "just above the cap");
    assert!(matches!(at(gate - 1), Action::Partial { .. }), "just under the gate");
    assert_eq!(at(gate), Action::Refresh, "healthy at the gate, and stale by 25 > 16");

    // The ratchet: a tick at or below the vault's last_height decides nothing.
    let stale_tick = d.tick(LH, price);
    assert_eq!(decide(&vault(debt, floor - 1), &stale_tick, ANCHOR, REFRESH_LAG, FEE), Action::None);

    // The refresh lag boundary is strict.
    assert_eq!(
        decide(&vault(debt, gate), &d.tick(LH + REFRESH_LAG, price), ANCHOR, REFRESH_LAG, FEE),
        Action::None
    );
    assert_eq!(
        decide(&vault(debt, gate), &d.tick(LH + REFRESH_LAG + 1, price), ANCHOR, REFRESH_LAG, FEE),
        Action::Refresh
    );

    // A debtless husk is never actionable.
    assert_eq!(decide(&vault(0, 1_000_000), &tick, ANCHOR, REFRESH_LAG, FEE), Action::None);
}

/// The planner lands the residual inside the band with the extraction at (or under) the
/// cap, and one more dd unit breaks it - the one-sat-tight edge, mirrored from M9.
#[test]
fn plan_partial_maximizes_dd_within_the_heal_band() {
    let (debt, coll, hi) = (4_000_000u64, 100_000_000u64, Price::new(50_000));
    let (dd, residual) = plan_partial(Obol::new(debt), Sats::new(coll), hi, FEE).expect("plannable");
    let rd = (debt - dd.raw()) as u32;
    let lo_band = coll_at_cr(rd, hi, K_HEAL_LO).raw();
    let hi_band = coll_at_cr(rd, hi, K_HEAL_HI).raw();
    assert!(residual.raw() >= lo_band && residual.raw() <= hi_band, "inside the band");
    let extraction = coll - residual.raw();
    let cap = coll_at_cr(dd.raw() as u32, hi, K_FULL_LIQ_CAP).raw();
    assert!(extraction <= cap, "the owner loses at most 1.15 x dd");

    // dd + 1 with the planner's residual formula must not fit any more.
    let dd1 = dd.raw() + 1;
    let cap1 = coll_at_cr(dd1 as u32, hi, K_FULL_LIQ_CAP).raw();
    let hi_band1 = coll_at_cr((debt - dd1) as u32, hi, K_HEAL_HI).raw();
    assert!(coll.saturating_sub(cap1) > hi_band1, "dd is maximal");
}

proptest! {
    /// Whatever the ladder says, the named checked builder accepts - and for a partial, one
    /// more dd unit is refused.
    #[test]
    fn every_verdict_is_builder_accepted(
        debt in 10_000u64..50_000_000,
        price in 1_000u32..500_000, // past the ~220k quasi-monotonicity regime of plan_partial
        // Collateral as a per-mille ratio of debt-at-par: sweeps under water through healthy.
        ratio_pm in 500u64..1_600,
    ) {
        let d = TestDeploy::get();
        let hi = Price::new(price);
        let debt_sats = coll_at_cr(debt as u32, hi, K_PAR).raw();
        let coll = debt_sats * ratio_pm / 1_000;
        prop_assume!(coll > 0);
        let v = vault(debt, coll);
        let tick = d.tick(TICK_H, price);
        let protocol = protocol_state(u32::MAX as u64, 100_000_000_000, LH);
        let keeper_spk = op_true_spk();
        let obol = |value: u64| ObolCoin {
            outpoint: synthetic_outpoint(0x60),
            value: Obol::new(value),
            spk: keeper_spk.clone(),
        };
        let fee_coin = FundingCoin {
            outpoint: synthetic_outpoint(0x61),
            value: Sats::new(1_000_000),
            spk: keeper_spk.clone(),
        };

        match decide(&v, &tick, ANCHOR, REFRESH_LAG, FEE) {
            Action::BadDebt => {
                let intent = BadDebtIntent {
                    keeper: obol(debt),
                    keeper_spk: keeper_spk.clone(),
                    obol_change_spk: keeper_spk.clone(),
                    fee_coin,
                    change_spk: keeper_spk.clone(),
                    tick,
                    fee: FEE,
                };
                prop_assert!(build::bad_debt::bad_debt(&d.ctx, &protocol, &v, &intent).is_ok());
            }
            Action::FullLiq => {
                let intent = FullLiqIntent {
                    keeper: obol(debt + 1),
                    keeper_spk: keeper_spk.clone(),
                    obol_change_spk: keeper_spk.clone(),
                    tick,
                    fee: FEE,
                };
                prop_assert!(build::full_liq::full_liq(&d.ctx, &protocol, &v, &intent).is_ok());
            }
            Action::Partial { dd, residual } => {
                let intent = |dd: Obol, residual: Sats| LiquidateIntent {
                    dd,
                    residual,
                    keeper: obol(dd.raw()),
                    keeper_spk: keeper_spk.clone(),
                    obol_change_spk: keeper_spk.clone(),
                    tick: tick.clone(),
                    fee: FEE,
                };
                prop_assert!(
                    build::liquidate::liquidate(&d.ctx, &protocol, &v, &intent(dd, residual)).is_ok()
                );
                // Maximality: dd + 1 with its own max-extraction residual must be refused.
                let dd1 = dd.raw() + 1;
                let residual1 = if dd1 < debt {
                    let lo_band = coll_at_cr((debt - dd1) as u32, hi, K_HEAL_LO).raw();
                    let cap1 = coll_at_cr(dd1 as u32, hi, K_FULL_LIQ_CAP).raw();
                    Sats::new(lo_band.max(coll.saturating_sub(cap1)))
                } else {
                    Sats::new(0)
                };
                prop_assert!(
                    build::liquidate::liquidate(&d.ctx, &protocol, &v, &intent(Obol::new(dd1), residual1))
                        .is_err()
                );
            }
            Action::Refresh => {
                let intent = RefreshIntent {
                    tick,
                    fee_coin,
                    change_spk: keeper_spk.clone(),
                    fee: FEE,
                };
                prop_assert!(build::refresh::refresh(&d.ctx, &v, &intent).is_ok());
            }
            Action::None => {
                // Healthy-and-fresh (lag is 25 > 16 here, so only unplannable partials land
                // here); nothing to hold against a builder.
            }
        }
    }
}

/// A diverging quorum: the min quote puts the vault under water, the max quote in the
/// partial band - liquidations price at the MAX quote, so the ladder must too.
#[test]
fn the_ladder_judges_by_the_max_quote() {
    let d = TestDeploy::get();
    let tick = tick_diverging(d, TICK_H, [35_000, 42_000, 50_000]);
    let a = decide(&vault(4_000_000, 100_000_000), &tick, ANCHOR, REFRESH_LAG, FEE);
    assert!(matches!(a, Action::Partial { .. }), "judged at hi=50k, not lo=35k: {a:?}");
}

/// A bad-debt verdict needs the tick at or above the issuer anchor (the ATTEST arm's
/// check_tick); the other liquidations carry no anchor gate.
#[test]
fn bad_debt_respects_the_issuer_anchor() {
    let d = TestDeploy::get();
    let tick = d.tick(TICK_H, 50_000);
    let deep = vault(4_000_000, 10_000_000); // far under water
    assert_eq!(
        decide(&deep, &tick, BlockHeight::new(TICK_H + 1), REFRESH_LAG, FEE),
        Action::None,
        "an under-anchor tick cannot attest"
    );
    assert_eq!(decide(&deep, &tick, BlockHeight::new(TICK_H), REFRESH_LAG, FEE), Action::BadDebt);
    // Full-liq has no anchor gate: the same over-anchor tick still acts.
    let cap = coll_at_cr(4_000_000, Price::new(50_000), K_FULL_LIQ_CAP).raw();
    assert_eq!(
        decide(&vault(4_000_000, cap), &tick, BlockHeight::new(TICK_H + 1), REFRESH_LAG, FEE),
        Action::FullLiq
    );
}

/// In the partial band with a debt too small to pay for its own liquidation, the ladder
/// says None - and the builders indeed refuse every dd at its best-extraction residual.
#[test]
fn an_unprofitable_partial_band_decides_none_and_nothing_builds() {
    let d = TestDeploy::get();
    let (debt, price) = (1_000u64, 120_000u32); // $10 of debt: extraction < share + fee
    let hi = Price::new(price);
    let cap_full = coll_at_cr(debt as u32, hi, K_FULL_LIQ_CAP).raw();
    let gate = coll_at_cr(debt as u32, hi, K_HEALTH_GATE).raw();
    let coll = (cap_full + gate) / 2;
    assert!(coll > cap_full && coll < gate, "mid partial band");
    let tick = d.tick(TICK_H, price);
    assert_eq!(decide(&vault(debt, coll), &tick, ANCHOR, REFRESH_LAG, FEE), Action::None);
    assert!(plan_partial(Obol::new(debt), Sats::new(coll), hi, FEE).is_none());

    // The planner's None is the builders' no: for every dd, the max-extraction residual
    // (band floor, cap-clamped) is refused - smaller extractions only fail harder.
    let protocol = protocol_state(u32::MAX as u64, 100_000_000_000, LH);
    let keeper_spk = op_true_spk();
    for dd in 1..debt {
        let band_lo = coll_at_cr((debt - dd) as u32, hi, K_HEAL_LO).raw();
        let cap = coll_at_cr(dd as u32, hi, K_FULL_LIQ_CAP).raw();
        let residual = band_lo.max(coll.saturating_sub(cap));
        let intent = LiquidateIntent {
            dd: Obol::new(dd),
            residual: Sats::new(residual),
            keeper: ObolCoin {
                outpoint: synthetic_outpoint(0x62),
                value: Obol::new(dd),
                spk: keeper_spk.clone(),
            },
            keeper_spk: keeper_spk.clone(),
            obol_change_spk: keeper_spk.clone(),
            tick: tick.clone(),
            fee: FEE,
        };
        assert!(
            build::liquidate::liquidate(&d.ctx, &protocol, &vault(debt, coll), &intent).is_err(),
            "dd={dd} must not build"
        );
    }
}
