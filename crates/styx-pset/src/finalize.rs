//! Finalization: prune each covenant input against the fixed transaction and attach the
//! 4-element Simplicity script witness [witness, program, cmr script, control block].
//!
//! `satisfy_with_env` gives the same accept/reject verdict the node gives, so this is both
//! the production path and the prune-tier test oracle. Slots are processed in the plan's
//! order over the progressively witnessed transaction, matching the prototype's defensive
//! ordering (signature-free covenants first).

use std::collections::HashMap;
use std::sync::Arc;

use styx_core::artifacts::{cmr_script, PotLeaf};
use styx_core::elements::taproot::ControlBlock;
use styx_core::elements::Transaction;
use styx_core::encode::{issuer_op_value, stability_op_value};
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
    }
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
