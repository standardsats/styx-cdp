//! Partial LIQUIDATE (permissionless, heal-to-target): open only below the 130% gate, the
//! keeper repays `dd` and heals the residual into [132%, 137%] at the max quote; the owner
//! loses at most 1.15 x dd, the penalty splits keeper 10% / reserve 5%.
//!
//! Layout: inputs [vault(0), pot(1), keeper OBOL(2), reserve(3)];
//!         outputs [residual vault(0), pot + dd(1), keeper collateral(2),
//!                  reserve + 5% share(3), (keeper OBOL change), fee]

use styx_core::consts::{K_FULL_LIQ_CAP, K_HEALTH_GATE, K_HEAL_HI, K_HEAL_LO, K_RESERVE_SHARE};
use styx_core::domain::{OnChain, PotState, ProtocolState, ReserveState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{StabilityOp, VaultOp};
use styx_core::math::coll_at_cr;
use styx_core::units::{Obol, Sats};

use crate::error::BuildError;
use crate::intent::LiquidateIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct LiquidateDelta {
    pub vault: OnChain<VaultState>,
    pub pot: OnChain<PotState>,
    pub reserve: OnChain<ReserveState>,
}

pub fn liquidate(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &LiquidateIntent,
) -> Result<Built<LiquidateDelta>, BuildError> {
    super::check_zero_price(&intent.tick)?;
    super::check_vault_ratchet(&intent.tick, vault.state.last_height)?;
    let (_, hi) = intent.tick.price_range();
    let debt_cents = vault.state.debt.covenant_cents()?;
    let gate = coll_at_cr(debt_cents, hi, K_HEALTH_GATE);
    if vault.value >= gate {
        return Err(BuildError::VaultTooHealthy { gate, have: vault.value });
    }
    if intent.dd == Obol::ZERO {
        return Err(BuildError::ZeroAmount);
    }
    // Strictly partial: dd == debt would collapse the heal band to [0, 0] and the covenant
    // requires a positive residual debt.
    if intent.dd >= vault.state.debt {
        return Err(BuildError::NotPartial { dd: intent.dd, debt: vault.state.debt });
    }
    let rd = vault.state.debt.checked_sub(intent.dd)?.covenant_cents()?;
    let (lo_band, hi_band) = (coll_at_cr(rd, hi, K_HEAL_LO), coll_at_cr(rd, hi, K_HEAL_HI));
    if intent.residual < lo_band || intent.residual > hi_band {
        return Err(BuildError::HealOutOfBand { residual: intent.residual, lo: lo_band, hi: hi_band });
    }
    let extraction = vault.value.checked_sub(intent.residual)?;
    let cap = coll_at_cr(intent.dd.covenant_cents()?, hi, K_FULL_LIQ_CAP);
    if extraction > cap {
        return Err(BuildError::ExtractionExceedsCap { extraction, cap });
    }
    if intent.keeper.value < intent.dd {
        return Err(BuildError::InsufficientPayer { need: intent.dd, have: intent.keeper.value });
    }
    let share = coll_at_cr(intent.dd.covenant_cents()?, hi, K_RESERVE_SHARE);
    if extraction <= share.checked_add(intent.fee)? {
        return Err(BuildError::InsufficientFunding {
            need: share.checked_add(intent.fee)?,
            have: extraction,
        });
    }
    Ok(liquidate_unchecked(ctx, protocol, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn liquidate_unchecked(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &LiquidateIntent,
) -> Built<LiquidateDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let (_, hi) = intent.tick.price_range();
    let dd = intent.dd.raw();
    let share = coll_at_cr(intent.dd.covenant_cents().unwrap_or(u32::MAX), hi, K_RESERVE_SHARE).raw();
    let keeper_coll = vault
        .value
        .raw()
        .saturating_sub(intent.residual.raw())
        .saturating_sub(share)
        .saturating_sub(intent.fee.raw());
    let successor =
        VaultState { debt: Obol::new(vault.state.debt.raw().saturating_sub(dd)), ..vault.state };
    let pot_out = protocol.pot.value.raw().saturating_add(dd);
    let reserve_out = protocol.reserve.value.raw().saturating_add(share);
    let mut output = vec![
        txout(intent.residual.raw(), a.vault_spk(&successor), p.policy),
        txout(pot_out, a.pot_spk(), p.obol),
        txout(keeper_coll, intent.keeper_spk.clone(), p.policy),
        txout(reserve_out, a.stability_spk(), p.policy),
    ];
    if intent.keeper.value.raw() > dd {
        output.push(txout(intent.keeper.value.raw() - dd, intent.obol_change_spk.clone(), p.obol));
    }
    output.push(fee_out(intent.fee, p.policy));
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(intent.tick.height().raw()),
        input: vec![
            txin(vault.outpoint),
            txin(protocol.pot.outpoint),
            txin(intent.keeper.outpoint),
            txin(protocol.reserve.outpoint),
        ],
        output,
    };
    let in_utxos = vec![
        claimed(vault.value.raw(), a.vault_spk(&vault.state), p.policy),
        claimed(protocol.pot.value.raw(), a.pot_spk(), p.obol),
        claimed(intent.keeper.value.raw(), intent.keeper.spk.clone(), p.obol),
        claimed(protocol.reserve.value.raw(), a.stability_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::Accumulate) },
        WitnessSlot {
            input: 0,
            kind: SlotKind::Vault {
                state: vault.state,
                op: Box::new(VaultOp::Liquidate { dd: intent.dd, tick: intent.tick.clone() }),
            },
        },
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = LiquidateDelta {
        vault: OnChain { state: successor, outpoint: OutPoint::new(txid, 0), value: intent.residual },
        pot: OnChain { state: PotState, outpoint: OutPoint::new(txid, 1), value: Obol::new(pot_out) },
        reserve: OnChain {
            state: ReserveState,
            outpoint: OutPoint::new(txid, 3),
            value: Sats::new(reserve_out),
        },
    };
    Built { plan, expected }
}
