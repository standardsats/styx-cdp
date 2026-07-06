//! Out-of-process owner signing: the export format that lets a vault owner key live off the
//! app (a hardware device, an air-gapped machine). It is the M6/M10 seam made portable -
//! `vault_sighash` produces the digest, `install_owner_sig` puts the answer back - with a
//! serializable request in between so the two ends need not share a process.
//!
//! Only the OWNER signature travels this way. The funding key-spend is the app's own hot
//! key (`sign_funding`); the point of an external owner is that the key authorizing a vault
//! op never touches the online machine, while the fee-paying funding key can.

use serde::{Deserialize, Serialize};
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::encode::Sig;

use crate::error::PruneRejected;
use crate::finalize::vault_sighash;
use crate::plan::{SlotKind, TxPlan};
use crate::sign::install_owner_sig;
use crate::Ctx;

#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error(transparent)]
    Sighash(#[from] PruneRejected),
    #[error("this plan has no vault owner op to sign")]
    NoOwnerOp,
    #[error("bad signature encoding: {0}")]
    SigEncoding(&'static str),
    /// The signature does not verify against the vault owner over the exported digest -
    /// refused here, not left for the covenant to reject at broadcast.
    #[error("signature does not verify against the vault owner")]
    BadSignature,
}

/// What an external signer receives: the digest to sign and the key it must sign with, both
/// hex. `txid` pins the request to its transaction so a stale signature cannot be reapplied
/// to a different body (the sighash already commits the body; the txid makes a mismatch
/// legible before verification).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerSigningRequest {
    pub txid: String,
    /// BIP340 message: the Simplicity `sig_all_hash` over the fixed transaction body.
    pub sighash: String,
    /// The x-only owner key the covenant will verify against.
    pub owner: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the signing request for a plan's vault owner op, or `NoOwnerOp` for a plan that
/// carries none (a permissionless op, or a pure funding tx). The digest equals what
/// `owner_sign` would sign in process.
pub fn owner_signing_request(ctx: &Ctx, plan: &TxPlan) -> Result<OwnerSigningRequest, SigningError> {
    let owner = plan
        .slots
        .iter()
        .find_map(|s| match &s.kind {
            SlotKind::Vault { state, op } if op.is_owner_op() => Some(state.owner),
            _ => None,
        })
        .ok_or(SigningError::NoOwnerOp)?;
    let digest = vault_sighash(ctx, plan)?;
    Ok(OwnerSigningRequest {
        txid: plan.txid().to_string(),
        sighash: hex(&digest),
        owner: hex(&owner.serialize()),
    })
}

/// Verify an externally produced signature against the plan's owner and digest, then
/// install it. The verification is the same refuse-early guard `finalize_pset` applies to
/// funding signatures: a wrong device, a signature over a stale body, or a typo fails here
/// rather than at the node.
pub fn apply_owner_sig(ctx: &Ctx, plan: &mut TxPlan, sig_hex: &str) -> Result<(), SigningError> {
    let owner = plan
        .slots
        .iter()
        .find_map(|s| match &s.kind {
            SlotKind::Vault { state, op } if op.is_owner_op() => Some(state.owner),
            _ => None,
        })
        .ok_or(SigningError::NoOwnerOp)?;
    let bytes = decode_64(sig_hex)?;
    let sig = zkp::schnorr::Signature::from_slice(&bytes)
        .map_err(|_| SigningError::SigEncoding("not a schnorr signature"))?;
    let digest = vault_sighash(ctx, plan)?;
    let msg = zkp::Message::from_digest(digest);
    styx_core::secp().verify_schnorr(&sig, &msg, &owner).map_err(|_| SigningError::BadSignature)?;
    install_owner_sig(plan, Sig(bytes));
    Ok(())
}

fn decode_64(s: &str) -> Result<[u8; 64], SigningError> {
    if s.len() != 128 {
        return Err(SigningError::SigEncoding("expected 128 hex chars"));
    }
    let mut out = [0u8; 64];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| SigningError::SigEncoding("not hex"))?;
    }
    Ok(out)
}
