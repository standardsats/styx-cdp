//! CLOSE: repay the full debt, free the collateral. No oracle, no timelock; the tx fee comes
//! from the freed collateral.
//!
//! Layout: inputs [vault(0), pot(1), payer OBOL(2)];
//!         outputs [collateral - fee(0), pot + debt(1), (payer surplus), fee]

use styx_core::domain::{OnChain, PotState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{Sig, VaultOp};
use styx_core::units::Obol;

use crate::error::BuildError;
use crate::intent::CloseIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct CloseDelta {
    pub pot: OnChain<PotState>,
    pub freed: OutPoint,
}

pub fn close(
    ctx: &Ctx,
    pot: &OnChain<PotState>,
    vault: &OnChain<VaultState>,
    intent: &CloseIntent,
) -> Result<Built<CloseDelta>, BuildError> {
    if intent.payer.value < vault.state.debt {
        return Err(BuildError::InsufficientPayer { need: vault.state.debt, have: intent.payer.value });
    }
    if vault.value <= intent.fee {
        return Err(BuildError::InsufficientFunding { need: intent.fee, have: vault.value });
    }
    Ok(close_unchecked(ctx, pot, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn close_unchecked(
    ctx: &Ctx,
    pot: &OnChain<PotState>,
    vault: &OnChain<VaultState>,
    intent: &CloseIntent,
) -> Built<CloseDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let debt = vault.state.debt.raw();
    let mut output = vec![
        txout(
            vault.value.raw().saturating_sub(intent.fee.raw()),
            intent.recipient_spk.clone(),
            p.policy,
        ),
        txout(pot.value.raw().saturating_add(debt), a.pot_spk(), p.obol),
    ];
    if intent.payer.value.raw() > debt {
        output.push(txout(intent.payer.value.raw() - debt, intent.payer_change_spk.clone(), p.obol));
    }
    output.push(fee_out(intent.fee, p.policy));
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::ZERO,
        input: vec![txin(vault.outpoint), txin(pot.outpoint), txin(intent.payer.outpoint)],
        output,
    };
    let in_utxos = vec![
        claimed(vault.value.raw(), a.vault_spk(&vault.state), p.policy),
        claimed(pot.value.raw(), a.pot_spk(), p.obol),
        claimed(intent.payer.value.raw(), intent.payer.spk.clone(), p.obol),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot {
            input: 0,
            kind: SlotKind::Vault {
                state: vault.state,
                op: Box::new(VaultOp::Close { owner_sig: Sig([0u8; 64]) }),
            },
        },
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = CloseDelta {
        pot: OnChain {
            state: PotState,
            outpoint: OutPoint::new(txid, 1),
            value: Obol::new(pot.value.raw() + debt),
        },
        freed: OutPoint::new(txid, 0),
    };
    Built { plan, expected }
}
