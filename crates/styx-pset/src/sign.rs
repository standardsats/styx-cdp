//! Owner signing: builders emit owner ops with a placeholder signature (the sighash exists
//! only once the transaction body is fixed); this completes them.

use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::encode::Sig;

use crate::error::PruneRejected;
use crate::finalize::vault_sighash;
use crate::plan::{SlotKind, TxPlan};

/// Sign the plan's vault sighash with the owner key and install the signature in the vault
/// slot's op. `vault_sighash` is public for out-of-process signers (a wallet role signs the
/// digest and the caller installs it the same way).
pub fn owner_sign(
    ctx: &crate::Ctx,
    plan: &mut TxPlan,
    owner: &zkp::Keypair,
) -> Result<(), PruneRejected> {
    let digest = vault_sighash(ctx, plan)?;
    let msg = zkp::Message::from_digest(digest);
    let sig = Sig(*styx_core::secp().sign_schnorr_no_aux_rand(&msg, owner).as_ref());
    install_owner_sig(plan, sig);
    Ok(())
}

/// Install an externally produced signature over `vault_sighash`.
pub fn install_owner_sig(plan: &mut TxPlan, sig: Sig) {
    for slot in &mut plan.slots {
        if let SlotKind::Vault { op, .. } = &mut slot.kind {
            **op = op.clone().with_owner_sig(sig);
        }
    }
}
