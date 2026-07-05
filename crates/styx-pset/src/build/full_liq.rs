//! FULL LIQUIDATION (permissionless, CR in [100%, 115%] - the partial-liq dead zone, E-3).
//! The keeper repays the full debt and seizes the collateral; one third of the excess
//! (coll - debt_sats) goes to the reserve, the vault closes. No issuer involved.
//!
//! Layout: inputs [vault(0), pot(1), keeper OBOL(2), reserve(3)];
//!         outputs [keeper collateral(0), pot + debt(1), keeper OBOL change(2),
//!                  reserve + excess/3(3), fee]

use styx_core::consts::{K_FULL_LIQ_CAP, K_PAR};
use styx_core::domain::{OnChain, PotState, ProtocolState, ReserveState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{StabilityOp, VaultOp};
use styx_core::math::coll_at_cr;
use styx_core::units::{Obol, Sats};

use crate::error::BuildError;
use crate::intent::FullLiqIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct FullLiqDelta {
    pub pot: OnChain<PotState>,
    pub reserve: OnChain<ReserveState>,
    pub seized: OutPoint,
}

pub fn full_liq(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &FullLiqIntent,
) -> Result<Built<FullLiqDelta>, BuildError> {
    super::check_zero_price(&intent.tick)?;
    super::check_vault_ratchet(&intent.tick, vault.state.last_height)?;
    let (_, hi) = intent.tick.price_range();
    let debt_cents = vault.state.debt.covenant_cents()?;
    let floor = coll_at_cr(debt_cents, hi, K_PAR); // CR >= 100%
    let cap = coll_at_cr(debt_cents, hi, K_FULL_LIQ_CAP); // CR <= 115%
    if vault.value < floor || vault.value > cap {
        return Err(BuildError::CrOutOfBand { coll: vault.value, floor, cap });
    }
    // Strictly more than the debt: the positive OBOL change is the E-5 anchor.
    if intent.keeper.value <= vault.state.debt {
        return Err(BuildError::InsufficientPayer {
            need: vault.state.debt.checked_add(Obol::new(1))?,
            have: intent.keeper.value,
        });
    }
    Ok(full_liq_unchecked(ctx, protocol, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn full_liq_unchecked(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &FullLiqIntent,
) -> Built<FullLiqDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let (_, hi) = intent.tick.price_range();
    let debt = vault.state.debt.raw();
    let debt_sats = coll_at_cr(vault.state.debt.covenant_cents().unwrap_or(u32::MAX), hi, K_PAR);
    let reserve_fee = vault.value.raw().saturating_sub(debt_sats.raw()) / 3;
    let pot_out = protocol.pot.value.raw().saturating_add(debt);
    let reserve_out = protocol.reserve.value.raw().saturating_add(reserve_fee);
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(intent.tick.height().raw()),
        input: vec![
            txin(vault.outpoint),
            txin(protocol.pot.outpoint),
            txin(intent.keeper.outpoint),
            txin(protocol.reserve.outpoint),
        ],
        output: vec![
            txout(
                vault.value.raw().saturating_sub(reserve_fee).saturating_sub(intent.fee.raw()),
                intent.keeper_spk.clone(),
                p.policy,
            ),
            txout(pot_out, a.pot_spk(), p.obol),
            txout(
                intent.keeper.value.raw().saturating_sub(debt),
                intent.obol_change_spk.clone(),
                p.obol,
            ),
            txout(reserve_out, a.stability_spk(), p.policy),
            fee_out(intent.fee, p.policy),
        ],
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
                op: Box::new(VaultOp::FullLiq { tick: intent.tick.clone() }),
            },
        },
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = FullLiqDelta {
        pot: OnChain { state: PotState, outpoint: OutPoint::new(txid, 1), value: Obol::new(pot_out) },
        reserve: OnChain {
            state: ReserveState,
            outpoint: OutPoint::new(txid, 3),
            value: Sats::new(reserve_out),
        },
        seized: OutPoint::new(txid, 0),
    };
    Built { plan, expected }
}
