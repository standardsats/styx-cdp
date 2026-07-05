//! Finalization: prune each covenant input against the fixed transaction and attach the
//! 4-element Simplicity script witness [witness, program, cmr script, control block].
//!
//! `satisfy_with_env` gives the same accept/reject verdict the node gives, so this is both
//! the production path and the prune-tier test oracle. Slots are processed in the plan's
//! order over the progressively witnessed transaction (defensive ordering: signature-free
//! covenants first).

use std::collections::HashMap;
use std::sync::Arc;

use styx_core::artifacts::{cmr_script, PotLeaf};
use styx_core::elements::hashes::Hash as _;
use styx_core::elements::taproot::ControlBlock;
use styx_core::elements::Transaction;
use styx_core::encode::{issuer_op_value, stability_op_value, vault_op_value};
use styx_core::params::xonly_u256;
use styx_core::simplicity::jet::elements::{ElementsEnv, ElementsUtxo};
use styx_core::simplicityhl::str::WitnessName;
use styx_core::simplicityhl::value::ValueConstructible;
use styx_core::simplicityhl::{CompiledProgram, Value, WitnessValues};

use crate::error::PruneRejected;
use crate::plan::{SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

/// Witness every covenant slot and return the broadcastable transaction.
pub fn finalize(ctx: &Ctx, plan: &TxPlan) -> Result<Transaction, PruneRejected> {
    let mut tx = plan.tx.clone();
    for slot in &plan.slots {
        let witness = slot_witness(ctx, &tx, &plan.in_utxos, slot)?;
        tx.input[slot.input as usize].witness.script_witness = witness;
    }
    Ok(tx)
}

/// The verdict and witness for a single slot against the given transaction state. Exposed so
/// negative tests can attribute a rejection to one covenant input, independent of slot order.
pub fn slot_witness(
    ctx: &Ctx,
    tx: &Transaction,
    in_utxos: &[ElementsUtxo],
    slot: &WitnessSlot,
) -> Result<Vec<Vec<u8>>, PruneRejected> {
    let a = &ctx.artifacts;
    match &slot.kind {
        // The pot leaves have no witness values and no case nodes: satisfy without an env.
        SlotKind::PotInflow => leaf_witness(
            &a.reserve_repay,
            a.pot_control_block(PotLeaf::Inflow),
            slot.input,
            "reserve_repay",
        ),
        SlotKind::PotOutflow => leaf_witness(
            &a.pot_outflow,
            a.pot_control_block(PotLeaf::Outflow),
            slot.input,
            "pot_outflow",
        ),
        SlotKind::Stability(op) => {
            let cb = a.stability_control_block();
            let wv = witness_values(vec![("OP", stability_op_value(op))]);
            pruned_witness(ctx, &a.stability, cb, tx, in_utxos, slot.input, wv, "stability")
        }
        SlotKind::Issuer { state, op } => {
            let cb = a.issuer_control_block(state);
            let wv = witness_values(vec![
                ("LAST_MINT_HEIGHT", Value::u32(state.last_mint_height.raw())),
                ("OP", issuer_op_value(op)),
            ]);
            pruned_witness(ctx, &a.issuer, cb, tx, in_utxos, slot.input, wv, "issuer")
        }
        SlotKind::Vault { state, op } => {
            let cb = a.vault_control_block(state);
            let wv = witness_values(vec![
                ("DEBT", Value::u64(state.debt.raw())),
                ("OWNER", Value::u256(xonly_u256(&state.owner))),
                ("LAST_HEIGHT", Value::u32(state.last_height.raw())),
                ("OP", vault_op_value(op)),
            ]);
            pruned_witness(ctx, &a.vault, cb, tx, in_utxos, slot.input, wv, "vault")
        }
    }
}

/// The transaction with every slot before the one at `input` witnessed, in plan order - the
/// state the covenant at `input` is verified against during `finalize`.
pub fn tx_before_slot(ctx: &Ctx, plan: &TxPlan, input: u32) -> Result<Transaction, PruneRejected> {
    let mut tx = plan.tx.clone();
    for slot in &plan.slots {
        if slot.input == input {
            break;
        }
        let witness = slot_witness(ctx, &tx, &plan.in_utxos, slot)?;
        tx.input[slot.input as usize].witness.script_witness = witness;
    }
    Ok(tx)
}

/// The sighash the vault owner signs, for the plan's vault slot. Computed against the same
/// transaction state `finalize` verifies against, so the signature survives finalization
/// regardless of whether the sighash commits to sibling witnesses.
pub fn vault_sighash(ctx: &Ctx, plan: &TxPlan) -> Result<[u8; 32], PruneRejected> {
    let (input, state) = plan
        .slots
        .iter()
        .find_map(|s| match &s.kind {
            SlotKind::Vault { state, .. } => Some((s.input, *state)),
            _ => None,
        })
        .ok_or(PruneRejected { input: 0, covenant: "vault", message: "no vault slot".into() })?;
    let tx = tx_before_slot(ctx, plan, input)?;
    let env = ElementsEnv::new(
        Arc::new(tx),
        plan.in_utxos.clone(),
        input,
        ctx.artifacts.vault.commit().cmr(),
        ctx.artifacts.vault_control_block(&state),
        None,
        ctx.genesis,
    );
    Ok(env.c_tx_env().sighash_all().to_byte_array())
}

fn witness_values(pairs: Vec<(&str, Value)>) -> WitnessValues {
    WitnessValues::from(
        pairs
            .into_iter()
            .map(|(n, v)| (WitnessName::from_str_unchecked(n), v))
            .collect::<HashMap<_, _>>(),
    )
}

fn stack(
    prog: &CompiledProgram,
    program_bytes: Vec<u8>,
    witness_bytes: Vec<u8>,
    cb: ControlBlock,
) -> Vec<Vec<u8>> {
    vec![witness_bytes, program_bytes, cmr_script(prog).into_bytes(), cb.serialize()]
}

fn leaf_witness(
    prog: &CompiledProgram,
    cb: ControlBlock,
    input: u32,
    covenant: &'static str,
) -> Result<Vec<Vec<u8>>, PruneRejected> {
    let satisfied = prog.satisfy(WitnessValues::default()).map_err(|message| PruneRejected {
        input,
        covenant,
        message,
    })?;
    let (program_bytes, witness_bytes) = satisfied.redeem().to_vec_with_witness();
    Ok(stack(prog, program_bytes, witness_bytes, cb))
}

#[allow(clippy::too_many_arguments)]
fn pruned_witness(
    ctx: &Ctx,
    prog: &CompiledProgram,
    cb: ControlBlock,
    tx: &Transaction,
    in_utxos: &[ElementsUtxo],
    input: u32,
    wv: WitnessValues,
    covenant: &'static str,
) -> Result<Vec<Vec<u8>>, PruneRejected> {
    let env = ElementsEnv::new(
        Arc::new(tx.clone()),
        in_utxos.to_vec(),
        input,
        prog.commit().cmr(),
        cb.clone(),
        None,
        ctx.genesis,
    );
    let pruned = prog.satisfy_with_env(wv, Some(&env)).map_err(|e| PruneRejected {
        input,
        covenant,
        message: e.to_string(),
    })?;
    let (program_bytes, witness_bytes) = pruned.redeem().to_vec_with_witness();
    Ok(stack(prog, program_bytes, witness_bytes, cb))
}
