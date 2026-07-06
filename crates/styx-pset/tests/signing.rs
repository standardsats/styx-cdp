//! Out-of-process owner signing: the request an external device receives, the digest it
//! signs, and the verify-then-install on the way back - the round trip must land exactly
//! where the in-process `owner_sign` does.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::elements::secp256k1_zkp as zkp;
use styx_pset::signing::{apply_owner_sig, owner_signing_request, SigningError};
use styx_pset::testkit::scenarios;
use styx_pset::testkit::{assert_accepts, keypair, TestDeploy};

/// A raw owner signature over an arbitrary digest, the way a hardware device would answer.
fn sign(owner: &zkp::Keypair, digest: &str) -> String {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&digest[i * 2..i * 2 + 2], 16).unwrap();
    }
    let msg = zkp::Message::from_digest(bytes);
    styx_core::secp().sign_schnorr_no_aux_rand(&msg, owner).to_string()
}

#[test]
fn an_external_owner_signature_completes_the_op() {
    let d = TestDeploy::get();
    let s = scenarios::close(d); // an owner op, built with a placeholder signature

    // The app exports the request; the digest is exactly what the covenant will verify.
    let req = owner_signing_request(&d.ctx, &s.plan).expect("has an owner op");
    assert_eq!(req.owner, hex(&s.owner.x_only_public_key().0.serialize()));
    assert_eq!(req.txid, s.plan.txid().to_string());

    // The request survives serialization to and from the wire.
    let json = serde_json::to_string(&req).unwrap();
    let back: styx_pset::signing::OwnerSigningRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, req);

    // The device signs the digest; the app verifies and installs it.
    let signature = sign(&s.owner, &req.sighash);
    let mut plan = s.plan.clone();
    apply_owner_sig(&d.ctx, &mut plan, &signature).expect("verifies and installs");

    // The externally signed plan finalizes exactly like an in-process one.
    assert_accepts(d, &plan);
}

#[test]
fn a_signature_from_the_wrong_key_is_refused_before_the_node() {
    let d = TestDeploy::get();
    let s = scenarios::close(d);
    let req = owner_signing_request(&d.ctx, &s.plan).unwrap();
    let signature = sign(&keypair(77), &req.sighash); // a stranger's key
    let mut plan = s.plan.clone();
    assert!(matches!(apply_owner_sig(&d.ctx, &mut plan, &signature), Err(SigningError::BadSignature)));
}

#[test]
fn a_malformed_signature_is_a_typed_error() {
    let d = TestDeploy::get();
    let s = scenarios::close(d);
    let mut plan = s.plan.clone();
    assert!(matches!(apply_owner_sig(&d.ctx, &mut plan, "cafe"), Err(SigningError::SigEncoding(_))));
}

#[test]
fn a_permissionless_op_has_no_owner_to_sign() {
    // A refresh scenario carries no owner op: the request is NoOwnerOp, not a bad digest.
    let d = TestDeploy::get();
    let s = scenarios::refresh(d);
    assert!(matches!(owner_signing_request(&d.ctx, &s.plan), Err(SigningError::NoOwnerOp)));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_multibyte_signature_string_is_a_typed_error_not_a_panic() {
    // A 128-BYTE (not 128-char) string with multibyte UTF-8 would slice on a non-char
    // boundary and panic; it must be a clean SigEncoding error.
    let d = TestDeploy::get();
    let s = scenarios::close(d);
    let mut plan = s.plan.clone();
    let multibyte = "\u{00e9}".repeat(64); // 64 chars, 128 bytes, non-ascii
    assert_eq!(multibyte.len(), 128);
    assert!(matches!(apply_owner_sig(&d.ctx, &mut plan, &multibyte), Err(SigningError::SigEncoding(_))));
}
