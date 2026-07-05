//! The PSET layer: emission, explicit-output enforcement, E-5 keeper payout binding, and
//! golden digests per op family.

use styx_core::elements::confidential;
use styx_core::elements::hashes::{sha256, Hash};
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::Address;
use styx_core::elements::SchnorrSighashType;
use styx_pset::pset::{finalize_pset, funding_sighash, sign_funding, to_pset, PsetError};
use styx_pset::testkit::scenarios;
use styx_pset::testkit::{assert_accepts, keypair, poke_intent, protocol_state, TestDeploy};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A key-owned p2tr output for the given secret (key-path only, no script tree).
fn key_spk(secret: u8) -> styx_core::elements::Script {
    let xonly = keypair(secret).x_only_public_key().0;
    Address::p2tr(styx_core::secp(), xonly, None, None, &styx_core::elements::AddressParams::ELEMENTS)
        .script_pubkey()
}

#[test]
fn pset_extracts_the_plan_transaction() {
    let d = TestDeploy::get();
    let s = scenarios::open_plan(d);
    let pset = to_pset(&d.ctx, &s).expect("emits");
    assert_eq!(pset.extract_tx().expect("extracts").txid(), s.txid());
    assert_eq!(pset.inputs().len(), s.tx.input.len());
    for input in pset.inputs() {
        assert!(input.witness_utxo.is_some());
    }
}

#[test]
fn pset_rejects_a_non_explicit_output() {
    let d = TestDeploy::get();
    let plan = scenarios::open_plan(d).tamper(|tx, _| {
        // Null stands in for the whole non-explicit class (same is_explicit() path as a
        // confidential commitment); real blinding is out of scope.
        tx.output[0].value = confidential::Value::Null;
    });
    assert!(matches!(to_pset(&d.ctx, &plan), Err(PsetError::NonExplicitOutput { index: 0 })));
}

#[test]
fn keeper_funding_signed_sighash_all_binds_payout() {
    // E-5: the keeper's funding signature is SIGHASH_ALL, so it commits the (destination-
    // unpinned) payout output. Redirect the payout and the signature no longer verifies
    // against the new sighash.
    let d = TestDeploy::get();
    let kp = keypair(77);
    let mut intent = scenarios::liquidate_intent(d.tick(120, 63_000));
    intent.keeper.spk = key_spk(77);
    intent.keeper_spk = key_spk(77);
    let s = scenarios::liquidate_funded(d, intent);
    let mut pset = to_pset(&d.ctx, &s).expect("emits");
    sign_funding(&d.ctx, &s, &mut pset, 2, &kp).expect("signs");
    let tx = finalize_pset(&d.ctx, &s, &pset).expect("finalizes");
    assert_eq!(tx.input[2].witness.script_witness.len(), 1, "key-spend witness installed");

    // The genuine signature verifies against the genuine digest (under the tweaked output
    // key, as any verifier would check it).
    let sig = pset.inputs()[2].tap_key_sig.expect("signed");
    let genuine = funding_sighash(&d.ctx, &s, 2).expect("sighash");
    let xonly = zkp::XOnlyPublicKey::from_slice(&key_spk(77).as_bytes()[2..34]).expect("output key");
    styx_core::secp()
        .verify_schnorr(&sig.sig, &zkp::Message::from_digest(genuine), &xonly)
        .expect("genuine payout verifies");

    // Redirect the keeper payout: the SIGHASH_ALL digest moves, the signature dies.
    let redirected = s.tamper(|tx, _| {
        tx.output[2].script_pubkey = key_spk(78);
    });
    let moved = funding_sighash(&d.ctx, &redirected, 2).expect("sighash");
    assert_ne!(genuine, moved);
    assert!(styx_core::secp()
        .verify_schnorr(&sig.sig, &zkp::Message::from_digest(moved), &xonly)
        .is_err());
}

#[test]
fn finalize_refuses_a_non_all_sighash() {
    // Constructive E-5: sign_funding only emits ALL, and a foreign signature with any other
    // type is refused at finalization.
    let d = TestDeploy::get();
    let s = scenarios::open_plan(d);
    let mut pset = to_pset(&d.ctx, &s).expect("emits");
    sign_funding(&d.ctx, &s, &mut pset, 2, &keypair(30)).expect("signs");
    assert_eq!(pset.inputs()[2].tap_key_sig.as_ref().map(|s| s.hash_ty), Some(SchnorrSighashType::All));

    let mut sig = pset.inputs()[2].tap_key_sig.expect("signed");
    sig.hash_ty = SchnorrSighashType::None;
    pset.inputs_mut()[2].tap_key_sig = Some(sig);
    assert!(matches!(finalize_pset(&d.ctx, &s, &pset), Err(PsetError::NonAllSighash { input: 2, .. })));
}

// Recorded from the first green run under the deterministic fixture: sha256 of the
// serialized PSET, one per op family. They churn on any layout or field change.
const PSET_DIGESTS: [(&str, &str); 10] = [
    ("poke", "81b1d61c38f481d63f3f3e71002ab24ec6418c67c1409b7242f2a912efb6f96b"),
    ("open", "44a30b7c4be24bba5d12494523aa151a383150680beec57fd83a278f8e1637a2"),
    ("close", "fe487b23ef69c32e5790704d82615a532bf0395c65c4736e2057967d86a6eece"),
    ("repay", "c94223c921db1cfb7ed92c58a0f18b45fcb99383cc91a18cd0535bc6f4dd9c3f"),
    ("draw", "ce36b8a9d1d835fd5226cd41d56dded9540bf3f68b8e95833a118b801dd5c610"),
    ("refresh", "ddcb4b690dd214a370b45b41afae006eaed979ee8511ab4ddcdec015b4a738aa"),
    ("liquidate", "4637450570bacbd4931c2a13e542d2c8eba0a98a6fa00e5f0fba519b361c74d6"),
    ("full_liq", "16a9db981b739575f7f15221a60feaacc5695e342f3f21933eacc6998593f5d9"),
    ("bad_debt", "8f2e28083e5f50ed7ded8c08bed739f94ba3cc06e9c73dd1c25bfe1a70a2be13"),
    ("redeem", "bf1d7002bf0d508c5cc6cebb744b104549a610418a10eafe2b2d3ca321750b4e"),
];

#[test]
fn finalize_refuses_a_bad_funding_signature() {
    // A wrong-key signature carries the right sighash type but does not verify against the
    // input's output key: refused at finalization, not at broadcast.
    let d = TestDeploy::get();
    let mut intent = scenarios::liquidate_intent(d.tick(120, 63_000));
    intent.keeper.spk = key_spk(77);
    intent.keeper_spk = key_spk(77);
    let s = scenarios::liquidate_funded(d, intent);
    let mut pset = to_pset(&d.ctx, &s).expect("emits");
    sign_funding(&d.ctx, &s, &mut pset, 2, &keypair(78)).expect("signs");
    assert!(matches!(
        finalize_pset(&d.ctx, &s, &pset),
        Err(PsetError::InvalidFundingSignature { input: 2 })
    ));
}

#[test]
fn finalize_refuses_a_signature_on_a_non_p2tr_input() {
    // A key-spend signature installed on an input whose claimed spk is not a v1 witness
    // program (the e2e op_true funding shape): there is no output key to verify against.
    let d = TestDeploy::get();
    let state = protocol_state(100_000_000, 0, 100);
    let built = styx_pset::build::poke::poke(&d.ctx, &state.issuer, &poke_intent(d.tick(120, 120_000)))
        .expect("builds");
    let mut pset = to_pset(&d.ctx, &built.plan).expect("emits");
    let msg = zkp::Message::from_digest(funding_sighash(&d.ctx, &built.plan, 1).expect("sighash"));
    let sig = styx_core::secp().sign_schnorr_no_aux_rand(&msg, &keypair(10));
    pset.inputs_mut()[1].tap_key_sig =
        Some(styx_core::elements::schnorr::SchnorrSig { sig, hash_ty: SchnorrSighashType::All });
    assert!(matches!(
        finalize_pset(&d.ctx, &built.plan, &pset),
        Err(PsetError::InvalidFundingSignature { input: 1 })
    ));
}

#[test]
fn finalize_pset_differs_from_finalize_only_by_funding_witnesses() {
    // The invariant behind the PSET path: it adds key-spend witnesses on the signed inputs
    // and changes nothing else relative to the raw finalize.
    let d = TestDeploy::get();
    let kp = keypair(77);
    let mut intent = scenarios::liquidate_intent(d.tick(120, 63_000));
    intent.keeper.spk = key_spk(77);
    intent.keeper_spk = key_spk(77);
    let s = scenarios::liquidate_funded(d, intent);
    let mut pset = to_pset(&d.ctx, &s).expect("emits");
    sign_funding(&d.ctx, &s, &mut pset, 2, &kp).expect("signs");

    let raw = styx_pset::finalize::finalize(&d.ctx, &s).expect("raw finalize");
    let via_pset = finalize_pset(&d.ctx, &s, &pset).expect("pset finalize");
    assert_eq!(raw.txid(), via_pset.txid());
    for i in 0..raw.input.len() {
        if i == 2 {
            assert!(raw.input[i].witness.script_witness.is_empty());
            assert_eq!(via_pset.input[i].witness.script_witness.len(), 1);
        } else {
            assert_eq!(
                raw.input[i].witness.script_witness, via_pset.input[i].witness.script_witness,
                "input {i} must be identical between the two paths"
            );
        }
    }
}

#[test]
fn pset_digests_golden() {
    let d = TestDeploy::get();
    let plans = [
        ("poke", {
            let state = protocol_state(100_000_000, 0, 100);
            styx_pset::build::poke::poke(&d.ctx, &state.issuer, &poke_intent(d.tick(120, 120_000)))
                .expect("builds")
                .plan
        }),
        ("open", scenarios::open_plan(d)),
        ("close", scenarios::close(d).plan),
        ("repay", scenarios::repay(d).plan),
        ("draw", scenarios::draw(d).plan),
        ("refresh", scenarios::refresh(d).plan),
        ("liquidate", scenarios::liquidate(d).plan),
        ("full_liq", scenarios::full_liq(d).plan),
        ("bad_debt", scenarios::bad_debt(d).plan),
        ("redeem", scenarios::redeem(d).plan),
    ];
    let mut mismatches = Vec::new();
    for ((name, plan), (gname, expected)) in plans.into_iter().zip(PSET_DIGESTS) {
        assert_eq!(name, gname);
        assert_accepts(d, &plan); // the golden is only meaningful for an accepting plan
        let pset = to_pset(&d.ctx, &plan).expect("emits");
        let bytes = styx_core::elements::encode::serialize(&pset);
        let digest = hex(&sha256::Hash::hash(&bytes).to_byte_array());
        if digest != expected {
            mismatches.push(format!("(\"{name}\", \"{digest}\"),"));
        }
    }
    assert!(mismatches.is_empty(), "PSET digests changed:\n{}", mismatches.join("\n"));
}
