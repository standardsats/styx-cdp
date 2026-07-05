//! Witness-encoder conformance against the compiled covenants.
//!
//! Two layers:
//! 1. Type equality: the `ToSimf`-derived type of each OP sum equals the witness type the
//!    covenant declares. Catches covenant/encoder drift directly, without constructing values.
//! 2. Per-variant satisfy: `CompiledProgram::satisfy` type-checks the encoded value of every
//!    op variant against the declared witness types (no spend env, no execution).
//!
//! Neither layer proves arm selection - two arms can share a type-compatible shape. That is
//! M5's discrimination matrix.

mod common;

use std::collections::HashMap;

use common::{test_artifacts, xonly};
use simplicityhl::str::WitnessName;
use simplicityhl::value::ValueConstructible;
use simplicityhl::{Value, WitnessValues};

use styx_core::encode::{
    issuer_op_value, lower_tick, stability_op_value, vault_op_value, Either, IssuerOp, IssuerOpSum,
    StabilityOp, StabilityOpSum, ToSimf, VaultOp, VaultOpSum,
};
use styx_core::oracle::{OracleSlot, OracleTick, SignedQuote};
use styx_core::units::{BlockHeight, Obol, Price, RatioK};
use styx_core::U256;

fn w(name: &str, v: Value) -> (WitnessName, Value) {
    (WitnessName::from_str_unchecked(name), v)
}

/// Type conformance does not verify signatures, so the dummy-sig tick suffices.
fn tick() -> OracleTick {
    common::dummy_tick()
}

fn sig() -> styx_core::encode::Sig {
    styx_core::encode::Sig([0u8; 64])
}

fn owner_u256() -> U256 {
    U256::from_byte_array(xonly(10).serialize())
}

// --- layer 1: declared witness types == ToSimf types --------------------------

#[test]
fn vault_witness_types_match() {
    let template = styx_core::artifacts::template(styx_core::artifacts::Covenant::Vault).unwrap();
    let types = template.witness_types();
    assert_eq!(types.get(&WitnessName::from_str_unchecked("OP")), Some(&VaultOpSum::ty()));
    assert_eq!(types.get(&WitnessName::from_str_unchecked("DEBT")), Some(&u64::ty()));
    assert_eq!(types.get(&WitnessName::from_str_unchecked("OWNER")), Some(&U256::ty()));
    assert_eq!(types.get(&WitnessName::from_str_unchecked("LAST_HEIGHT")), Some(&u32::ty()));
}

#[test]
fn issuer_witness_types_match() {
    let template = styx_core::artifacts::template(styx_core::artifacts::Covenant::Issuer).unwrap();
    let types = template.witness_types();
    assert_eq!(types.get(&WitnessName::from_str_unchecked("OP")), Some(&IssuerOpSum::ty()));
    assert_eq!(
        types.get(&WitnessName::from_str_unchecked("LAST_MINT_HEIGHT")),
        Some(&u32::ty())
    );
}

#[test]
fn stability_witness_types_match() {
    let template = styx_core::artifacts::template(styx_core::artifacts::Covenant::Stability).unwrap();
    let types = template.witness_types();
    assert_eq!(types.get(&WitnessName::from_str_unchecked("OP")), Some(&StabilityOpSum::ty()));
}

// --- layer 2: every variant satisfies its covenant ----------------------------

fn vault_satisfies(op: &VaultOp) {
    let wv = WitnessValues::from(HashMap::from([
        w("DEBT", Value::u64(5_000_000)),
        w("OWNER", Value::u256(owner_u256())),
        w("LAST_HEIGHT", Value::u32(100)),
        w("OP", vault_op_value(op)),
    ]));
    test_artifacts().vault.satisfy(wv).unwrap_or_else(|e| panic!("{op:?}: {e}"));
}

#[test]
fn every_vault_op_variant_satisfies() {
    let t = tick();
    for op in [
        VaultOp::Close { owner_sig: sig() },
        VaultOp::Repay { owner_sig: sig(), amount: Obol::new(1_000) },
        VaultOp::Draw { owner_sig: sig(), amount: Obol::new(1_000), tick: t.clone() },
        VaultOp::Liquidate { dd: Obol::new(1_000), tick: t.clone() },
        VaultOp::FullLiq { tick: t.clone() },
        VaultOp::BadDebt { tick: t.clone() },
        VaultOp::Redeem { x: Obol::new(1_000), tick: t.clone() },
        VaultOp::Refresh { tick: t.clone() },
    ] {
        vault_satisfies(&op);
    }
}

fn issuer_satisfies(op: &IssuerOp) {
    let wv = WitnessValues::from(HashMap::from([
        w("LAST_MINT_HEIGHT", Value::u32(100)),
        w("OP", issuer_op_value(op)),
    ]));
    test_artifacts().issuer.satisfy(wv).unwrap_or_else(|e| panic!("{op:?}: {e}"));
}

#[test]
fn every_issuer_op_variant_satisfies() {
    let t = tick();
    for op in [
        IssuerOp::Open { principal: Obol::new(1_000), owner: owner_u256(), tick: t.clone() },
        IssuerOp::Draw {
            old_debt: Obol::new(1_000),
            owner: owner_u256(),
            old_last_height: BlockHeight::new(90),
            new_debt: Obol::new(2_000),
            draw_height: BlockHeight::new(100),
        },
        IssuerOp::Poke { tick: t.clone() },
        IssuerOp::Attest {
            debt: Obol::new(1_000),
            owner: owner_u256(),
            last_height: BlockHeight::new(90),
            tick: t.clone(),
        },
    ] {
        issuer_satisfies(&op);
    }
}

#[test]
fn every_stability_op_variant_satisfies() {
    for op in [StabilityOp::Accumulate, StabilityOp::BadDebt] {
        let wv = WitnessValues::from(HashMap::from([w("OP", stability_op_value(&op))]));
        test_artifacts().stability.satisfy(wv).unwrap_or_else(|e| panic!("{op:?}: {e}"));
    }
}

#[test]
fn vault_rejects_a_mistyped_op_value() {
    // An issuer OP value under the vault's OP name: satisfy must reject it, which is what
    // makes the positive satisfy tests above meaningful.
    let wv = WitnessValues::from(HashMap::from([
        w("DEBT", Value::u64(5_000_000)),
        w("OWNER", Value::u256(owner_u256())),
        w("LAST_HEIGHT", Value::u32(100)),
        w("OP", issuer_op_value(&IssuerOp::Poke { tick: tick() })),
    ]));
    assert!(test_artifacts().vault.satisfy(wv).is_err());
}

// --- tick lowering -------------------------------------------------------------

#[test]
fn lowered_tick_places_quotes_in_their_slots() {
    let quote = SignedQuote { price: Price::new(63_000), sig: [7u8; 64] };
    let t = OracleTick::new(
        BlockHeight::new(200),
        RatioK::from_cr_percent(100),
        [
            (OracleSlot::new(1).unwrap(), quote),
            (OracleSlot::new(3).unwrap(), quote),
            (OracleSlot::new(4).unwrap(), quote),
        ],
    )
    .unwrap();
    let (height, (backing_k, slots)) = lower_tick(&t);
    assert_eq!(height, 200);
    assert_eq!(backing_k, 200_000_000);
    let active: Vec<bool> = slots.iter().map(|s| matches!(s, Either::Right(_))).collect();
    assert_eq!(active, [false, true, false, true, true]);
    match &slots[1] {
        Either::Right((price, s)) => {
            assert_eq!(*price, 63_000);
            assert_eq!(s.0, [7u8; 64]);
        }
        Either::Left(()) => panic!("slot 1 must be active"),
    }
}
