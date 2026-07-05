//! End-to-end amount parity at the heal-band edges: for random liquidation scenarios the
//! builder's residual sits exactly on the accepting side of the covenant's gates. This is
//! what "same truncation direction" buys - a one-sat disagreement between the Rust math and
//! the covenant would surface here as an edge case that flips.
//!
//! The "one sat deeper" pair exercises whichever gate binds for the draw:
//! `residual = lo.max(coll - cap)` sits on the band floor when the floor binds and on the
//! extraction cap when the cap binds, so both gates get one-sat-tightness coverage. Keep the
//! max() when refactoring - flattening it to `lo` silently drops the cap cases.
//!
//! Each case is several full prunes, so the default case count stays at 64 (the heaviest
//! test of the fast tier); raise it per run with PROPTEST_CASES when needed. The high-volume
//! direct parity property lives in styx-core/tests/parity.rs.

use proptest::prelude::*;
use styx_core::consts::{K_FULL_LIQ_CAP, K_HEALTH_GATE, K_HEAL_HI, K_HEAL_LO, K_RESERVE_SHARE};
use styx_core::domain::{OnChain, VaultState};
use styx_core::math::coll_at_cr;
use styx_core::units::{BlockHeight, Obol, Price, Sats};
use styx_pset::build::liquidate::liquidate_unchecked;
use styx_pset::finalize::finalize;
use styx_pset::intent::{LiquidateIntent, ObolCoin};
use styx_pset::testkit::{keypair, op_true_spk, protocol_state, synthetic_outpoint, TestDeploy, FEE};

fn accepts(d: &TestDeploy, vault: &OnChain<VaultState>, intent: &LiquidateIntent) -> bool {
    let protocol = protocol_state(95_000_000, 1_000_000, 100);
    let built = liquidate_unchecked(&d.ctx, &protocol, vault, intent);
    finalize(&d.ctx, &built.plan).is_ok()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn heal_band_edges_are_tight(
        debt in 2_000_000u32..=8_000_000,
        price in 50_000u32..=70_000,
        dd_permille in 100u32..=300, // dd as a fraction of the debt, 10% - 30%
        coll_offset in 0u64..=2_000_000, // how far under the 130% gate the vault sits
    ) {
        let d = TestDeploy::get();
        let p = Price::new(price);
        let dd = (debt as u64) * (dd_permille as u64) / 1000;
        let rd = debt - dd as u32;
        let gate = coll_at_cr(debt, p, K_HEALTH_GATE).raw();
        let coll = gate - 1 - coll_offset; // strictly below the gate
        let (lo, hi) = (coll_at_cr(rd, p, K_HEAL_LO).raw(), coll_at_cr(rd, p, K_HEAL_HI).raw());
        let cap = coll_at_cr(dd as u32, p, K_FULL_LIQ_CAP).raw();
        let share = coll_at_cr(dd as u32, p, K_RESERVE_SHARE).raw();

        // The keeper heals as deep as the covenant allows: the band floor, unless the
        // extraction cap forces a shallower cut. Discard infeasible draws.
        let residual = lo.max(coll.saturating_sub(cap));
        prop_assume!(residual <= hi);
        prop_assume!(residual < coll);
        prop_assume!(coll - residual > share + FEE.raw()); // the keeper payout stays positive

        let owner = keypair(10);
        let vault = OnChain {
            state: VaultState {
                debt: Obol::new(debt as u64),
                owner: owner.x_only_public_key().0,
                last_height: BlockHeight::new(100),
            },
            outpoint: synthetic_outpoint(0xC0),
            value: Sats::new(coll),
        };
        let intent = |residual: u64| LiquidateIntent {
            dd: Obol::new(dd),
            residual: Sats::new(residual),
            keeper: ObolCoin {
                outpoint: synthetic_outpoint(0xB4),
                value: Obol::new(dd),
                spk: op_true_spk(),
            },
            keeper_spk: op_true_spk(),
            obol_change_spk: op_true_spk(),
            tick: d.tick(120, price),
            fee: FEE,
        };

        prop_assert!(accepts(d, &vault, &intent(residual)), "the edge residual must accept");
        prop_assert!(!accepts(d, &vault, &intent(residual - 1)), "one sat deeper must reject");
        if hi < coll && coll - hi > share + FEE.raw() {
            prop_assert!(accepts(d, &vault, &intent(hi)), "the band ceiling must accept");
            prop_assert!(!accepts(d, &vault, &intent(hi + 1)), "one sat above the ceiling must reject");
        }
    }
}
