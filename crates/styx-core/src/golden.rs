//! Freeze gate: golden CMRs of the five frozen v1 covenants.
//!
//! Each covenant is compiled against a fixed set of dummy params so its CMR isolates the covenant
//! source logic. The CMR does not commit to comments or witness values, so doc-only edits leave it
//! unchanged; any change that alters a covenant's behaviour flips its CMR and fails
//! `frozen_cmrs_match_v1_freeze`. The vendored `covenants/*.simf` must stay behaviourally
//! identical to the `v1-covenant-freeze` tag of the prototype repo, where these five hex values
//! were recorded (2026-07-03); this test is what holds them to it.
//!
//! To re-freeze after a reviewed covenant change: run
//! `cargo test -p styx-core golden -- --nocapture`, confirm the change was intended, and paste
//! the new hex below.

use simplicityhl::str::WitnessName;
use simplicityhl::value::ValueConstructible;
use simplicityhl::{Arguments, Value};
use std::collections::HashMap;

use crate::artifacts::{cmr_u256, compile, Covenant};
use crate::consts::{issuer_leaf_vc, leaf_vc, nums_key, tapleaf_tag};
use crate::U256;

fn w(name: &str, v: Value) -> (WitnessName, Value) {
    (WitnessName::from_str_unchecked(name), v)
}
fn args(pairs: Vec<(WitnessName, Value)>) -> Arguments {
    Arguments::from(pairs.into_iter().collect::<HashMap<_, _>>())
}
/// A dummy 32-byte config param (asset id / scriptHash / key / cmr). The byte value does not
/// matter; it only has to stay fixed so the CMR tracks the source rather than the deploy config.
fn d(byte: u8) -> Value {
    Value::u256(U256::from_byte_array([byte; 32]))
}
fn nums() -> Value {
    Value::u256(U256::from_byte_array(nums_key().serialize()))
}
fn hex32(u: U256) -> String {
    u.to_byte_array().iter().map(|b| format!("{b:02x}")).collect()
}
fn oracle(m: &mut Vec<(WitnessName, Value)>) {
    m.push(w("ORACLE_PK_1", d(0x10)));
    m.push(w("ORACLE_PK_2", d(0x11)));
    m.push(w("ORACLE_PK_3", d(0x12)));
    m.push(w("ORACLE_PK_4", d(0x13)));
    m.push(w("ORACLE_PK_5", d(0x14)));
}

/// Compile all five covenants under the dummy params, in the canonical order.
/// Cached: compilation is the expensive step, and every golden test wants the same result.
fn cmrs() -> &'static [(&'static str, String)] {
    static CMRS: std::sync::OnceLock<Vec<(&'static str, String)>> = std::sync::OnceLock::new();
    CMRS.get_or_init(compute_cmrs)
}

fn compute_cmrs() -> Vec<(&'static str, String)> {
    let rr = compile(Covenant::ReserveRepay, args(vec![w("OBOL_ID", d(0x01))])).expect("reserve_repay");
    let po = compile(
        Covenant::PotOutflow,
        args(vec![w("OBOL_ID", d(0x01)), w("ISSUER_TOKEN_ID", d(0x02))]),
    )
    .expect("pot_outflow");
    let st = compile(
        Covenant::Stability,
        args(vec![
            w("POT_SPK", d(0x04)),
            w("ISSUER_TOKEN_ID", d(0x02)),
            w("OBOL_ID", d(0x01)),
            w("POLICY", d(0x03)),
        ]),
    )
    .expect("stability");
    let mut vb = vec![
        w("NUMS", nums()),
        w("TAPLEAF_TAG", Value::u256(tapleaf_tag())),
        w("LEAF_VC", Value::u16(leaf_vc())),
        w("OBOL_ID", d(0x01)),
        w("POLICY", d(0x03)),
        w("POT_SPK", d(0x04)),
        w("STABILITY_SPK", d(0x05)),
    ];
    oracle(&mut vb);
    let va = compile(Covenant::Vault, args(vb)).expect("vault");
    let mut ib = vec![
        w("NUMS", nums()),
        w("TAPLEAF_TAG", Value::u256(tapleaf_tag())),
        w("LEAF_VC", Value::u16(leaf_vc())),
        w("ISSUER_LEAF_VC", Value::u16(issuer_leaf_vc())),
        w("VAULT_CMR", d(0x06)),
        w("STABILITY_SPK", d(0x05)),
        w("OBOL_ID", d(0x01)),
        w("POLICY", d(0x03)),
        w("POT_SPK", d(0x04)),
        w("ISSUER_TOKEN_ID", d(0x02)),
    ];
    oracle(&mut ib);
    let is = compile(Covenant::Issuer, args(ib)).expect("issuer");
    vec![
        ("reserve_repay", hex32(cmr_u256(&rr))),
        ("pot_outflow", hex32(cmr_u256(&po))),
        ("stability", hex32(cmr_u256(&st))),
        ("vault", hex32(cmr_u256(&va))),
        ("issuer", hex32(cmr_u256(&is))),
    ]
}

/// Frozen v1 covenant CMRs (recorded 2026-07-03 at prototype tag `v1-covenant-freeze`).
/// See the module comment before changing.
const FROZEN: [(&str, &str); 5] = [
    ("reserve_repay", "52828127db4184834512b2e486e23c308f037e68deab8506fdcbb96e4b9e303f"),
    ("pot_outflow", "d1e8412dcf1fedaec9879e9afcaa1d2b11046178268b601d17c324716b47c5c6"),
    ("stability", "3ce24252af1248a0a6992868c324c8e928c55c4d043d85bf1cc4bbd8a1c65fd4"),
    ("vault", "8ea44eac4c31403a0e35b75940225d5f8833114ae2a9ac8d9607be25ec291eb7"),
    ("issuer", "145438e32272e966a1015593924210e7cc3951155ee2d904c81dee850a3b38f3"),
];

#[test]
fn frozen_cmrs_match_v1_freeze() {
    let got = cmrs();
    for (name, cmr) in got {
        println!("  {name:14} {cmr}");
    }
    for ((name, cmr), (fname, exp)) in got.iter().zip(FROZEN.iter()) {
        assert_eq!(name, fname, "covenant order mismatch");
        assert_eq!(
            cmr, exp,
            "covenant `{name}` CMR changed - freeze violated (or re-freeze after a reviewed change)"
        );
    }
}

#[test]
fn all_five_covenants_compile() {
    // cmrs() is cached, so this shares the compile work with the freeze test.
    assert_eq!(cmrs().len(), 5);
}
