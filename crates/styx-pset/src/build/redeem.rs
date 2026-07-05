//! REDEEM (permissionless, the peg floor): any OBOL holder swaps `x` for collateral worth x
//! at the max quote, valued at the backing floor min(par, backing_k) - under-backing caps the
//! extraction at the pro-rata share (E-2). The 0.5% fee stays par-priced and goes to the
//! reserve; the vault recurses to (debt - x, owner, last_height). No health gate.
//!
//! Layout: inputs [vault(0), pot(1), redeemer OBOL(2), reserve(3)];
//!         outputs [vault successor(0), pot + x(1), redeemer collateral(2),
//!                  reserve + fee(3), (redeemer OBOL change), fee]

use styx_core::consts::{K_FEE_HALF_PERCENT, K_PAR};
use styx_core::domain::{OnChain, PotState, ProtocolState, ReserveState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{StabilityOp, VaultOp};
use styx_core::math::coll_at_cr;
use styx_core::units::{Obol, Sats};

use crate::error::BuildError;
use crate::intent::RedeemIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct RedeemDelta {
    pub vault: OnChain<VaultState>,
    pub pot: OnChain<PotState>,
    pub reserve: OnChain<ReserveState>,
    pub redeemed: OutPoint,
}

pub fn redeem(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &RedeemIntent,
) -> Result<Built<RedeemDelta>, BuildError> {
    super::check_zero_price(&intent.tick)?;
    super::check_vault_ratchet(&intent.tick, vault.state.last_height)?;
    if intent.x == Obol::ZERO {
        return Err(BuildError::ZeroAmount);
    }
    if intent.x > vault.state.debt {
        return Err(BuildError::AmountExceedsDebt { amount: intent.x, debt: vault.state.debt });
    }
    if intent.redeemer.value < intent.x {
        return Err(BuildError::InsufficientPayer { need: intent.x, have: intent.redeemer.value });
    }
    let (_, hi) = intent.tick.price_range();
    let x_cents = intent.x.covenant_cents()?;
    let floor_k = intent.tick.backing_k().min(K_PAR);
    let x_worth = coll_at_cr(x_cents, hi, floor_k);
    if vault.value < x_worth {
        return Err(BuildError::InsufficientFunding { need: x_worth, have: vault.value });
    }
    let fee_share = coll_at_cr(x_cents, hi, K_FEE_HALF_PERCENT);
    if x_worth <= fee_share.checked_add(intent.fee)? {
        return Err(BuildError::InsufficientFunding {
            need: fee_share.checked_add(intent.fee)?,
            have: x_worth,
        });
    }
    Ok(redeem_unchecked(ctx, protocol, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn redeem_unchecked(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &RedeemIntent,
) -> Built<RedeemDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let (_, hi) = intent.tick.price_range();
    let x = intent.x.raw();
    let x_cents = intent.x.covenant_cents().unwrap_or(u32::MAX);
    let floor_k = intent.tick.backing_k().min(K_PAR);
    let x_worth = coll_at_cr(x_cents, hi, floor_k).raw();
    let fee_share = coll_at_cr(x_cents, hi, K_FEE_HALF_PERCENT).raw();
    let successor =
        VaultState { debt: Obol::new(vault.state.debt.raw().saturating_sub(x)), ..vault.state };
    let residual = vault.value.raw().saturating_sub(x_worth);
    let pot_out = protocol.pot.value.raw().saturating_add(x);
    let reserve_out = protocol.reserve.value.raw().saturating_add(fee_share);
    let mut output = vec![
        txout(residual, a.vault_spk(&successor), p.policy),
        txout(pot_out, a.pot_spk(), p.obol),
        txout(
            x_worth.saturating_sub(fee_share).saturating_sub(intent.fee.raw()),
            intent.redeemer_spk.clone(),
            p.policy,
        ),
        txout(reserve_out, a.stability_spk(), p.policy),
    ];
    if intent.redeemer.value.raw() > x {
        output.push(txout(intent.redeemer.value.raw() - x, intent.obol_change_spk.clone(), p.obol));
    }
    output.push(fee_out(intent.fee, p.policy));
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(intent.tick.height().raw()),
        input: vec![
            txin(vault.outpoint),
            txin(protocol.pot.outpoint),
            txin(intent.redeemer.outpoint),
            txin(protocol.reserve.outpoint),
        ],
        output,
    };
    let in_utxos = vec![
        claimed(vault.value.raw(), a.vault_spk(&vault.state), p.policy),
        claimed(protocol.pot.value.raw(), a.pot_spk(), p.obol),
        claimed(intent.redeemer.value.raw(), intent.redeemer.spk.clone(), p.obol),
        claimed(protocol.reserve.value.raw(), a.stability_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::Accumulate) },
        WitnessSlot {
            input: 0,
            kind: SlotKind::Vault {
                state: vault.state,
                op: Box::new(VaultOp::Redeem { x: intent.x, tick: intent.tick.clone() }),
            },
        },
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = RedeemDelta {
        vault: OnChain {
            state: successor,
            outpoint: OutPoint::new(txid, 0),
            value: Sats::new(residual),
        },
        pot: OnChain { state: PotState, outpoint: OutPoint::new(txid, 1), value: Obol::new(pot_out) },
        reserve: OnChain {
            state: ReserveState,
            outpoint: OutPoint::new(txid, 3),
            value: Sats::new(reserve_out),
        },
        redeemed: OutPoint::new(txid, 2),
    };
    Built { plan, expected }
}
