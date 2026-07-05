//! Math parity: the Rust `coll_at_cr` against the covenant's own arithmetic.
//!
//! The covenant function body is pasted verbatim into a shim program whose main asserts
//! `coll_at_cr(DEBT, PRICE, K) == EXPECTED`; `shim_is_verbatim_covenant_source` locks the
//! paste against drift. Executing the shim through `satisfy_with_env` (execution happens
//! during pruning) then gives the covenant's verdict on any (inputs, expected) pair: the
//! property holds when the Rust value is accepted and the Rust value plus one is not -
//! exact equality, not a bound.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use proptest::prelude::*;
use styx_core::consts::{
    K_BAD_DEBT_CAP, K_FEE_HALF_PERCENT, K_FULL_LIQ_CAP, K_HEALTH_GATE, K_HEAL_HI, K_HEAL_LO, K_OPEN_MIN,
    K_PAR, K_RESERVE_SHARE,
};
use styx_core::elements::hashes::Hash;
use styx_core::elements::taproot::TaprootBuilder;
use styx_core::elements::{
    confidential, AssetId, AssetIssuance, LockTime, OutPoint, Script, Sequence, Transaction, TxIn,
    TxInWitness, TxOut, TxOutWitness, Txid,
};
use styx_core::simplicity::jet::elements::{ElementsEnv, ElementsUtxo};
use styx_core::simplicityhl::ast::ElementsJetHinter;
use styx_core::simplicityhl::str::WitnessName;
use styx_core::simplicityhl::value::ValueConstructible;
use styx_core::simplicityhl::{Arguments, CompiledProgram, Value, WitnessValues};
use styx_core::units::{Price, RatioK};

/// The covenant's coll_at_cr, byte for byte (vault.simf:135-139).
const COVENANT_COLL_AT_CR: &str = "fn coll_at_cr(debt_cents: u32, price: u32, k: u32) -> u64 {
    let numerator: u64 = jet::multiply_32(debt_cents, k);
    let denom: u64 = jet::multiply_32(price, 200);
    jet::divide_64(numerator, denom)
}";

#[test]
fn shim_is_verbatim_covenant_source() {
    // The issuer carries its own copy of the function (SimplicityHL has no imports), and its
    // gates (150% at OPEN, the borrow fee, bounty, cap) compute with it - lock both copies,
    // so the parity below provably covers each.
    assert!(
        styx_core::artifacts::Covenant::Vault.source().contains(COVENANT_COLL_AT_CR),
        "the shim's coll_at_cr no longer matches the frozen vault.simf"
    );
    assert!(
        styx_core::artifacts::Covenant::Issuer.source().contains(COVENANT_COLL_AT_CR),
        "the shim's coll_at_cr no longer matches the frozen issuer.simf"
    );
}

// ElementsEnv holds raw C pointers and is not Sync, so the cache keeps its ingredients and
// the env is built per call.
struct Shim {
    prog: CompiledProgram,
    tx: Transaction,
    cb: styx_core::elements::taproot::ControlBlock,
}

fn shim() -> &'static Shim {
    static SHIM: OnceLock<Shim> = OnceLock::new();
    SHIM.get_or_init(|| {
        let src = format!(
            "{COVENANT_COLL_AT_CR}\n\nfn main() {{\n    let debt: u32 = witness::DEBT;\n    let price: u32 = witness::PRICE;\n    let k: u32 = witness::K;\n    let expected: u64 = witness::EXPECTED;\n    assert!(jet::eq_64(coll_at_cr(debt, price, k), expected));\n}}\n"
        );
        let prog = CompiledProgram::new(
            src.as_str(),
            Arguments::from(HashMap::new()),
            false,
            Box::new(ElementsJetHinter::new()),
        )
        .expect("shim compiles");
        // The shim reads nothing from the environment, so a minimal 1-in/1-out tx suffices.
        let policy = AssetId::from_slice(&[0x03; 32]).unwrap();
        let tx = Transaction {
            version: 2,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(Txid::from_slice(&[0xEE; 32]).unwrap(), 0),
                is_pegin: false,
                script_sig: Script::new(),
                sequence: Sequence::ZERO,
                asset_issuance: AssetIssuance::null(),
                witness: TxInWitness::empty(),
            }],
            output: vec![TxOut {
                value: confidential::Value::Explicit(1_000),
                script_pubkey: Script::from(vec![0x51]),
                asset: confidential::Asset::Explicit(policy),
                nonce: confidential::Nonce::Null,
                witness: TxOutWitness::empty(),
            }],
        };
        let leaf = Script::from(prog.commit().cmr().as_ref().to_vec());
        let ver = styx_core::simplicity::leaf_version();
        let info = TaprootBuilder::new()
            .add_leaf_with_ver(0, leaf.clone(), ver)
            .unwrap()
            .finalize(styx_core::secp(), styx_core::consts::nums_key())
            .unwrap();
        let cb = info.control_block(&(leaf, ver)).unwrap();
        Shim { prog, tx, cb }
    })
}

/// The covenant's verdict on coll_at_cr(debt, price, k) == expected.
fn shim_accepts(debt: u32, price: u32, k: u32, expected: u64) -> bool {
    let s = shim();
    let env = ElementsEnv::new(
        Arc::new(s.tx.clone()),
        vec![ElementsUtxo::from(s.tx.output[0].clone())],
        0,
        s.prog.commit().cmr(),
        s.cb.clone(),
        None,
        styx_core::elements::BlockHash::from_slice(&[0x42; 32]).unwrap(),
    );
    let w = |n: &str, v: Value| (WitnessName::from_str_unchecked(n), v);
    let wv = WitnessValues::from(HashMap::from([
        w("DEBT", Value::u32(debt)),
        w("PRICE", Value::u32(price)),
        w("K", Value::u32(k)),
        w("EXPECTED", Value::u64(expected)),
    ]));
    s.prog.satisfy_with_env(wv, Some(&env)).is_ok()
}

fn rust(debt: u32, price: u32, k: u32) -> u64 {
    styx_core::math::coll_at_cr(debt, Price::new(price), RatioK::new(k)).raw()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]
    #[test]
    fn coll_at_cr_matches_the_covenant_exactly(
        debt in prop_oneof![1u32..=100_000_000, 1u32..=u32::MAX],
        price in prop_oneof![1u32..=1_000, 30_000u32..=200_000, 1u32..=u32::MAX],
        k in prop_oneof![
            Just(K_FEE_HALF_PERCENT.raw()), Just(K_RESERVE_SHARE.raw()), Just(K_BAD_DEBT_CAP.raw()),
            Just(K_PAR.raw()), Just(K_FULL_LIQ_CAP.raw()), Just(K_HEALTH_GATE.raw()),
            Just(K_HEAL_LO.raw()), Just(K_HEAL_HI.raw()), Just(K_OPEN_MIN.raw()),
            1u32..=400_000_000,
        ],
    ) {
        let r = rust(debt, price, k);
        prop_assert!(shim_accepts(debt, price, k, r), "covenant disagrees at ({debt}, {price}, {k})");
        prop_assert!(!shim_accepts(debt, price, k, r + 1), "parity is not exact at ({debt}, {price}, {k})");
    }
}

#[test]
fn parity_at_the_domain_corners() {
    // Zero price: divide_64(n, 0) = 0 on both sides.
    assert_eq!(rust(5_000_000, 0, K_PAR.raw()), 0);
    assert!(shim_accepts(5_000_000, 0, K_PAR.raw(), 0));
    assert!(!shim_accepts(5_000_000, 0, K_PAR.raw(), 1));
    // The largest representable numerator: (2^32 - 1)^2 fits u64 exactly on both sides.
    let r = rust(u32::MAX, 1, u32::MAX);
    assert!(shim_accepts(u32::MAX, 1, u32::MAX, r));
    assert!(!shim_accepts(u32::MAX, 1, u32::MAX, r + 1));
    // Zero debt and zero k: a zero numerator is 0 on both sides.
    assert!(shim_accepts(0, 120_000, K_PAR.raw(), 0));
    assert!(shim_accepts(5_000_000, 120_000, 0, 0));
    assert!(!shim_accepts(5_000_000, 120_000, 0, 1));
}
