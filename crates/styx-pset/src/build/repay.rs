//! REPAY `amount`: reduce the debt; the covenant rejects any collateral reduction, so the tx
//! fee comes from a separate coin (the intent type carries no other option).
//!
//! Layout: inputs [vault(0), pot(1), payer OBOL(2), fee coin(3)];
//!         outputs [vault successor(0, collateral preserved), pot + amount(1),
//!                  (payer surplus), fee-coin change, fee]

use styx_core::domain::{OnChain, PotState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{Sig, VaultOp};
use styx_core::units::Obol;

use crate::error::BuildError;
use crate::intent::RepayIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct RepayDelta {
    pub vault: OnChain<VaultState>,
    pub pot: OnChain<PotState>,
}

pub fn repay(
    ctx: &Ctx,
    pot: &OnChain<PotState>,
    vault: &OnChain<VaultState>,
    intent: &RepayIntent,
) -> Result<Built<RepayDelta>, BuildError> {
    if intent.amount > vault.state.debt {
        return Err(BuildError::AmountExceedsDebt { amount: intent.amount, debt: vault.state.debt });
    }
    if intent.payer.value < intent.amount {
        return Err(BuildError::InsufficientPayer { need: intent.amount, have: intent.payer.value });
    }
    if intent.fee_coin.value <= intent.fee {
        return Err(BuildError::InsufficientFunding { need: intent.fee, have: intent.fee_coin.value });
    }
    Ok(repay_unchecked(ctx, pot, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn repay_unchecked(
    ctx: &Ctx,
    pot: &OnChain<PotState>,
    vault: &OnChain<VaultState>,
    intent: &RepayIntent,
) -> Built<RepayDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let r = intent.amount.raw();
    let successor =
        VaultState { debt: Obol::new(vault.state.debt.raw().saturating_sub(r)), ..vault.state };
    let mut output = vec![
        txout(vault.value.raw(), a.vault_spk(&successor), p.policy),
        txout(pot.value.raw().saturating_add(r), a.pot_spk(), p.obol),
    ];
    if intent.payer.value.raw() > r {
        output.push(txout(intent.payer.value.raw() - r, intent.payer_change_spk.clone(), p.obol));
    }
    output.push(txout(
        intent.fee_coin.value.raw().saturating_sub(intent.fee.raw()),
        intent.change_spk.clone(),
        p.policy,
    ));
    output.push(fee_out(intent.fee, p.policy));
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::ZERO,
        input: vec![
            txin(vault.outpoint),
            txin(pot.outpoint),
            txin(intent.payer.outpoint),
            txin(intent.fee_coin.outpoint),
        ],
        output,
    };
    let in_utxos = vec![
        claimed(vault.value.raw(), a.vault_spk(&vault.state), p.policy),
        claimed(pot.value.raw(), a.pot_spk(), p.obol),
        claimed(intent.payer.value.raw(), intent.payer.spk.clone(), p.obol),
        claimed(intent.fee_coin.value.raw(), intent.fee_coin.spk.clone(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot {
            input: 0,
            kind: SlotKind::Vault {
                state: vault.state,
                op: Box::new(VaultOp::Repay { owner_sig: Sig([0u8; 64]), amount: intent.amount }),
            },
        },
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = RepayDelta {
        vault: OnChain { state: successor, outpoint: OutPoint::new(txid, 0), value: vault.value },
        pot: OnChain {
            state: PotState,
            outpoint: OutPoint::new(txid, 1),
            value: Obol::new(pot.value.raw() + r),
        },
    };
    Built { plan, expected }
}
