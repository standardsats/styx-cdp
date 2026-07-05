//! BAD-DEBT liquidation (permissionless, issuer-attested; closes finding B). An underwater
//! vault (CR < 100% at the max quote) is fully repaid by the keeper; the reserve releases
//! min(shortfall + 5% bounty, 20% cap (M-1), its balance) so the keeper is made whole. The
//! issuer's ATTEST arm reconstructs the vault, sizes the payout, and authors the reserve
//! shrink; the reserve's bad-debt arm gates on the issuer token and defers the amount.
//!
//! Layout: inputs [vault(0), pot(1), keeper OBOL(2), reserve(3), issuer(4), fee coin(5)];
//!         outputs [keeper coll + reserve_pay(0), pot + debt(1), reserve - reserve_pay(2),
//!                  issuer successor(3), (keeper OBOL change), fee-coin change, fee]

use styx_core::consts::K_PAR;
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, ReserveState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{IssuerOp, StabilityOp, VaultOp};
use styx_core::math::{bad_debt_reserve_pay, coll_at_cr};
use styx_core::params::xonly_u256;
use styx_core::units::{Obol, Sats};

use crate::error::BuildError;
use crate::intent::BadDebtIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct BadDebtDelta {
    pub pot: OnChain<PotState>,
    pub reserve: OnChain<ReserveState>,
    pub issuer: OnChain<IssuerState>,
    pub seized: OutPoint,
}

pub fn bad_debt(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &BadDebtIntent,
) -> Result<Built<BadDebtDelta>, BuildError> {
    super::check_tick(&intent.tick, protocol.issuer.state.last_mint_height)?;
    super::check_vault_ratchet(&intent.tick, vault.state.last_height)?;
    let (_, hi) = intent.tick.price_range();
    let debt_cents = vault.state.debt.covenant_cents()?;
    let debt_sats = coll_at_cr(debt_cents, hi, K_PAR);
    if vault.value >= debt_sats {
        return Err(BuildError::NotUnderwater { debt_sats, coll: vault.value });
    }
    if intent.keeper.value < vault.state.debt {
        return Err(BuildError::InsufficientPayer { need: vault.state.debt, have: intent.keeper.value });
    }
    if intent.fee_coin.value <= intent.fee {
        return Err(BuildError::InsufficientFunding { need: intent.fee, have: intent.fee_coin.value });
    }
    Ok(bad_debt_unchecked(ctx, protocol, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn bad_debt_unchecked(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &BadDebtIntent,
) -> Built<BadDebtDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let (_, hi) = intent.tick.price_range();
    let debt = vault.state.debt.raw();
    let reserve_pay = bad_debt_reserve_pay(
        vault.state.debt.covenant_cents().unwrap_or(u32::MAX),
        hi,
        vault.value,
        protocol.reserve.value,
    )
    .raw();
    let issuer_succ = IssuerState { last_mint_height: intent.tick.height() };
    let pot_out = protocol.pot.value.raw().saturating_add(debt);
    let reserve_out = protocol.reserve.value.raw().saturating_sub(reserve_pay);
    let mut output = vec![
        txout(vault.value.raw().saturating_add(reserve_pay), intent.keeper_spk.clone(), p.policy),
        txout(pot_out, a.pot_spk(), p.obol),
        txout(reserve_out, a.stability_spk(), p.policy),
        txout(1, a.issuer_spk(&issuer_succ), p.issuer_token),
    ];
    if intent.keeper.value.raw() > debt {
        output.push(txout(intent.keeper.value.raw() - debt, intent.obol_change_spk.clone(), p.obol));
    }
    output.push(txout(
        intent.fee_coin.value.raw().saturating_sub(intent.fee.raw()),
        intent.change_spk.clone(),
        p.policy,
    ));
    output.push(fee_out(intent.fee, p.policy));
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(intent.tick.height().raw()),
        input: vec![
            txin(vault.outpoint),
            txin(protocol.pot.outpoint),
            txin(intent.keeper.outpoint),
            txin(protocol.reserve.outpoint),
            txin(protocol.issuer.outpoint),
            txin(intent.fee_coin.outpoint),
        ],
        output,
    };
    let in_utxos = vec![
        claimed(vault.value.raw(), a.vault_spk(&vault.state), p.policy),
        claimed(protocol.pot.value.raw(), a.pot_spk(), p.obol),
        claimed(intent.keeper.value.raw(), intent.keeper.spk.clone(), p.obol),
        claimed(protocol.reserve.value.raw(), a.stability_spk(), p.policy),
        claimed(1, a.issuer_spk(&protocol.issuer.state), p.issuer_token),
        claimed(intent.fee_coin.value.raw(), intent.fee_coin.spk.clone(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::BadDebt) },
        WitnessSlot {
            input: 4,
            kind: SlotKind::Issuer {
                state: protocol.issuer.state,
                op: Box::new(IssuerOp::Attest {
                    debt: vault.state.debt,
                    owner: xonly_u256(&vault.state.owner),
                    last_height: vault.state.last_height,
                    tick: intent.tick.clone(),
                }),
            },
        },
        WitnessSlot {
            input: 0,
            kind: SlotKind::Vault {
                state: vault.state,
                op: Box::new(VaultOp::BadDebt { tick: intent.tick.clone() }),
            },
        },
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = BadDebtDelta {
        pot: OnChain { state: PotState, outpoint: OutPoint::new(txid, 1), value: Obol::new(pot_out) },
        reserve: OnChain {
            state: ReserveState,
            outpoint: OutPoint::new(txid, 2),
            value: Sats::new(reserve_out),
        },
        issuer: OnChain { state: issuer_succ, outpoint: OutPoint::new(txid, 3), value: 1 },
        seized: OutPoint::new(txid, 0),
    };
    Built { plan, expected }
}
