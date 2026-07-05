//! REFRESH: advance the vault's freshness ratchet with a fresh tick, proving CR >= 130% at
//! the max quote (M-2: permissionless, so dormant healthy vaults can be kept liquidatable-
//! current by keepers). Collateral preserved; the fee comes from a separate coin.
//!
//! Layout: inputs [vault(0), fee coin(1)];
//!         outputs [vault successor(0, same collateral, new last_height), change(1), fee]

use styx_core::consts::K_HEALTH_GATE;
use styx_core::domain::{OnChain, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::VaultOp;
use styx_core::math::coll_at_cr;

use crate::error::BuildError;
use crate::intent::RefreshIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct RefreshDelta {
    pub vault: OnChain<VaultState>,
}

pub fn refresh(
    ctx: &Ctx,
    vault: &OnChain<VaultState>,
    intent: &RefreshIntent,
) -> Result<Built<RefreshDelta>, BuildError> {
    super::check_zero_price(&intent.tick)?;
    super::check_vault_ratchet(&intent.tick, vault.state.last_height)?;
    let (_, hi) = intent.tick.price_range();
    let need = coll_at_cr(vault.state.debt.covenant_cents()?, hi, K_HEALTH_GATE);
    if vault.value < need {
        return Err(BuildError::Undercollateralized { need, have: vault.value });
    }
    if intent.fee_coin.value <= intent.fee {
        return Err(BuildError::InsufficientFunding { need: intent.fee, have: intent.fee_coin.value });
    }
    Ok(refresh_unchecked(ctx, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn refresh_unchecked(
    ctx: &Ctx,
    vault: &OnChain<VaultState>,
    intent: &RefreshIntent,
) -> Built<RefreshDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let successor = VaultState { last_height: intent.tick.height(), ..vault.state };
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(intent.tick.height().raw()),
        input: vec![txin(vault.outpoint), txin(intent.fee_coin.outpoint)],
        output: vec![
            txout(vault.value.raw(), a.vault_spk(&successor), p.policy),
            txout(
                intent.fee_coin.value.raw().saturating_sub(intent.fee.raw()),
                intent.change_spk.clone(),
                p.policy,
            ),
            fee_out(intent.fee, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(vault.value.raw(), a.vault_spk(&vault.state), p.policy),
        claimed(intent.fee_coin.value.raw(), intent.fee_coin.spk.clone(), p.policy),
    ];
    let slots = vec![WitnessSlot {
        input: 0,
        kind: SlotKind::Vault {
            state: vault.state,
            op: Box::new(VaultOp::Refresh { tick: intent.tick.clone() }),
        },
    }];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = RefreshDelta {
        vault: OnChain { state: successor, outpoint: OutPoint::new(txid, 0), value: vault.value },
    };
    Built { plan, expected }
}
