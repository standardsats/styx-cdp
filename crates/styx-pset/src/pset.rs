//! PSET emission and finalization: the interop layer over `TxPlan`.
//!
//! A plan becomes a PSET with every input's `witness_utxo` and the covenant inputs' taproot
//! fields filled, so external wallets can inspect and sign their key-spend inputs. Covenant
//! witnesses never travel through PSET fields: they are pruned from the plan at finalization
//! and written as `final_script_witness`-shaped stacks directly into the transaction.
//!
//! E-5 (keeper payout binding) lives here: `sign_funding` only ever produces SIGHASH_ALL, so
//! a funding signature commits every output and the destination-unpinned keeper payout cannot
//! be redirected in the mempool; `finalize_pset` refuses any other sighash type.

use styx_core::elements::pset::PartiallySignedTransaction;
use styx_core::elements::schnorr::SchnorrSig;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::sighash::{Prevouts, SighashCache};
use styx_core::elements::SchnorrSighashType;
use styx_core::elements::{confidential, Transaction, TxOut, TxOutWitness};

use crate::error::PruneRejected;
use crate::finalize::finalize;
use crate::plan::TxPlan;
use crate::Ctx;

#[derive(Debug, thiserror::Error)]
pub enum PsetError {
    /// A non-explicit (confidential or null) output. Protocol outputs must be unblinded:
    /// the covenants' explicit-read jets fail on anything else.
    #[error("output {index} is not explicit")]
    NonExplicitOutput { index: u32 },
    /// A signature with a sighash type other than ALL. E-5: anything else would let a
    /// mempool observer redirect the destination-unpinned keeper payout.
    #[error("input {input} is signed with sighash type {found}, only ALL is allowed")]
    NonAllSighash { input: u32, found: String },
    /// A funding signature that does not verify against its input's output key. Refused here
    /// rather than at broadcast: a garbage signature from an external wallet fails early.
    #[error("input {input}: the funding signature does not verify")]
    InvalidFundingSignature { input: u32 },
    #[error("sighash computation failed: {0}")]
    Sighash(String),
    #[error("key tweak failed: {0}")]
    Tweak(String),
    #[error(transparent)]
    Prune(#[from] PruneRejected),
}

/// Every output must be explicit (the fee output included). Change is not exempt yet:
/// confidential change is out of scope until the protocol layouts get a blinding review.
pub fn validate_explicit(tx: &Transaction) -> Result<(), PsetError> {
    for (index, out) in tx.output.iter().enumerate() {
        if !out.value.is_explicit() || !out.asset.is_explicit() {
            return Err(PsetError::NonExplicitOutput { index: index as u32 });
        }
    }
    Ok(())
}

/// The plan as a PSET: the unsigned body, every input's `witness_utxo`, and the covenant
/// inputs' taproot metadata (internal key + the Simplicity leaf under its control block).
pub fn to_pset(ctx: &Ctx, plan: &TxPlan) -> Result<PartiallySignedTransaction, PsetError> {
    validate_explicit(&plan.tx)?;
    let mut pset = PartiallySignedTransaction::from_tx(plan.tx.clone());
    for (i, u) in plan.in_utxos.iter().enumerate() {
        pset.inputs_mut()[i].witness_utxo = Some(TxOut {
            asset: u.asset,
            value: u.value,
            nonce: confidential::Nonce::Null,
            script_pubkey: u.script_pubkey.clone(),
            witness: TxOutWitness::empty(),
        });
    }
    for slot in &plan.slots {
        let (cb, prog) = match &slot.kind {
            crate::plan::SlotKind::PotInflow => (
                ctx.artifacts.pot_control_block(styx_core::artifacts::PotLeaf::Inflow),
                &ctx.artifacts.reserve_repay,
            ),
            crate::plan::SlotKind::PotOutflow => (
                ctx.artifacts.pot_control_block(styx_core::artifacts::PotLeaf::Outflow),
                &ctx.artifacts.pot_outflow,
            ),
            crate::plan::SlotKind::Stability(_) => {
                (ctx.artifacts.stability_control_block(), &ctx.artifacts.stability)
            }
            crate::plan::SlotKind::Issuer { state, .. } => {
                (ctx.artifacts.issuer_control_block(state), &ctx.artifacts.issuer)
            }
            crate::plan::SlotKind::Vault { state, .. } => {
                (ctx.artifacts.vault_control_block(state), &ctx.artifacts.vault)
            }
        };
        let input = &mut pset.inputs_mut()[slot.input as usize];
        input.tap_internal_key = Some(styx_core::consts::nums_key());
        input
            .tap_scripts
            .insert(cb, (styx_core::artifacts::cmr_script(prog), styx_core::simplicity::leaf_version()));
    }
    Ok(pset)
}

/// The prevouts for sighash computation, from the plan's claimed UTXOs.
fn prevouts(plan: &TxPlan) -> Vec<TxOut> {
    plan.in_utxos
        .iter()
        .map(|u| TxOut {
            asset: u.asset,
            value: u.value,
            nonce: confidential::Nonce::Null,
            script_pubkey: u.script_pubkey.clone(),
            witness: TxOutWitness::empty(),
        })
        .collect()
}

/// The taproot key-spend sighash of a funding input, always SIGHASH_ALL.
pub fn funding_sighash(ctx: &Ctx, plan: &TxPlan, input: usize) -> Result<[u8; 32], PsetError> {
    use styx_core::elements::hashes::Hash;
    let outs = prevouts(plan);
    let mut cache = SighashCache::new(&plan.tx);
    let hash = cache
        .taproot_key_spend_signature_hash(
            input,
            &Prevouts::All(&outs),
            SchnorrSighashType::All,
            ctx.genesis,
        )
        .map_err(|e| PsetError::Sighash(e.to_string()))?;
    Ok(hash.to_byte_array())
}

/// Sign a key-spend funding input into the PSET. `keypair` is the wallet's internal key of a
/// key-path-only p2tr coin; the BIP341 tweak (empty script tree) is applied here, so the
/// signature verifies against the output key. Only SIGHASH_ALL is ever produced: the
/// signature commits every output (E-5).
pub fn sign_funding(
    ctx: &Ctx,
    plan: &TxPlan,
    pset: &mut PartiallySignedTransaction,
    input: usize,
    keypair: &zkp::Keypair,
) -> Result<(), PsetError> {
    use styx_core::elements::hashes::Hash;
    let tweak = styx_core::elements::taproot::TapTweakHash::from_key_and_tweak(
        keypair.x_only_public_key().0,
        None,
    );
    let scalar = zkp::Scalar::from_be_bytes(tweak.to_byte_array())
        .map_err(|e| PsetError::Tweak(e.to_string()))?;
    let tweaked = keypair
        .add_xonly_tweak(styx_core::secp(), &scalar)
        .map_err(|e| PsetError::Tweak(e.to_string()))?;
    let digest = funding_sighash(ctx, plan, input)?;
    let msg = zkp::Message::from_digest(digest);
    let sig = styx_core::secp().sign_schnorr_no_aux_rand(&msg, &tweaked);
    pset.inputs_mut()[input].tap_key_sig = Some(SchnorrSig { sig, hash_ty: SchnorrSighashType::All });
    Ok(())
}

/// Finalize: covenant witnesses pruned from the plan, key-spend witnesses taken from the
/// PSET's signatures, each verified against its input's output key and SIGHASH_ALL digest.
/// The output differs from `finalize`'s exactly by the signed inputs' witnesses.
pub fn finalize_pset(
    ctx: &Ctx,
    plan: &TxPlan,
    pset: &PartiallySignedTransaction,
) -> Result<Transaction, PsetError> {
    let mut tx = finalize(ctx, plan)?;
    for (i, input) in pset.inputs().iter().enumerate() {
        if let Some(sig) = &input.tap_key_sig {
            if sig.hash_ty != SchnorrSighashType::All {
                return Err(PsetError::NonAllSighash {
                    input: i as u32,
                    found: format!("{:?}", sig.hash_ty),
                });
            }
            // A key-path p2tr signature verifies against the tweaked output key: the last
            // 32 bytes of the v1 witness program.
            let spk = plan.in_utxos[i].script_pubkey.as_bytes();
            let bad = || PsetError::InvalidFundingSignature { input: i as u32 };
            if spk.len() != 34 || spk[0] != 0x51 || spk[1] != 0x20 {
                return Err(bad());
            }
            let output_key = zkp::XOnlyPublicKey::from_slice(&spk[2..34]).map_err(|_| bad())?;
            let digest = funding_sighash(ctx, plan, i)?;
            styx_core::secp()
                .verify_schnorr(&sig.sig, &zkp::Message::from_digest(digest), &output_key)
                .map_err(|_| bad())?;
            tx.input[i].witness.script_witness = vec![sig.to_vec()];
        }
    }
    Ok(tx)
}
