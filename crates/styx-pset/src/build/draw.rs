//! DRAW `amount` more OBOL: the pot releases it (issuer-gated), the vault recurses to the
//! higher debt at the tick height, 150% is re-checked on the post-draw collateral at the min
//! quote. The tx fee comes from the collateral.
//!
//! Layout: inputs [vault(0), pot(1), issuer(2)];
//!         outputs [vault successor(0), pot - amount(1), borrower(2), issuer successor(3), fee]

use styx_core::consts::K_OPEN_MIN;
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{IssuerOp, Sig, VaultOp};
use styx_core::math::coll_at_cr;
use styx_core::params::xonly_u256;
use styx_core::units::{Obol, Sats};

use crate::error::BuildError;
use crate::intent::DrawIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct DrawDelta {
    pub vault: OnChain<VaultState>,
    pub pot: OnChain<PotState>,
    pub issuer: OnChain<IssuerState>,
}

pub fn draw(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &DrawIntent,
) -> Result<Built<DrawDelta>, BuildError> {
    super::check_tick(&intent.tick, protocol.issuer.state.last_mint_height)?;
    super::check_vault_ratchet(&intent.tick, vault.state.last_height)?;
    if intent.amount == Obol::ZERO {
        return Err(BuildError::ZeroAmount);
    }
    if vault.value <= intent.fee {
        return Err(BuildError::InsufficientFunding { need: intent.fee, have: vault.value });
    }
    if intent.amount > protocol.pot.value {
        return Err(BuildError::InsufficientPot { need: intent.amount, have: protocol.pot.value });
    }
    let new_debt = vault.state.debt.checked_add(intent.amount)?;
    let (lo, _) = intent.tick.price_range();
    let need = coll_at_cr(new_debt.covenant_cents()?, lo, K_OPEN_MIN);
    let post = Sats::new(vault.value.raw().saturating_sub(intent.fee.raw()));
    if post < need {
        return Err(BuildError::Undercollateralized { need, have: post });
    }
    Ok(draw_unchecked(ctx, protocol, vault, intent))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn draw_unchecked(
    ctx: &Ctx,
    protocol: &ProtocolState,
    vault: &OnChain<VaultState>,
    intent: &DrawIntent,
) -> Built<DrawDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let d = intent.amount.raw();
    let h = intent.tick.height();
    let successor = VaultState {
        debt: Obol::new(vault.state.debt.raw().saturating_add(d)),
        owner: vault.state.owner,
        last_height: h,
    };
    let issuer_succ = IssuerState { last_mint_height: h };
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(h.raw()),
        input: vec![txin(vault.outpoint), txin(protocol.pot.outpoint), txin(protocol.issuer.outpoint)],
        output: vec![
            txout(vault.value.raw().saturating_sub(intent.fee.raw()), a.vault_spk(&successor), p.policy),
            txout(protocol.pot.value.raw().saturating_sub(d), a.pot_spk(), p.obol),
            txout(d, intent.borrower_spk.clone(), p.obol),
            txout(1, a.issuer_spk(&issuer_succ), p.issuer_token),
            fee_out(intent.fee, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(vault.value.raw(), a.vault_spk(&vault.state), p.policy),
        claimed(protocol.pot.value.raw(), a.pot_spk(), p.obol),
        claimed(1, a.issuer_spk(&protocol.issuer.state), p.issuer_token),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotOutflow },
        WitnessSlot {
            input: 2,
            kind: SlotKind::Issuer {
                state: protocol.issuer.state,
                op: Box::new(IssuerOp::Draw {
                    old_debt: vault.state.debt,
                    owner: xonly_u256(&vault.state.owner),
                    old_last_height: vault.state.last_height,
                    new_debt: successor.debt,
                    draw_height: h,
                }),
            },
        },
        WitnessSlot {
            input: 0,
            kind: SlotKind::Vault {
                state: vault.state,
                op: Box::new(VaultOp::Draw {
                    owner_sig: Sig([0u8; 64]),
                    amount: intent.amount,
                    tick: intent.tick.clone(),
                }),
            },
        },
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = DrawDelta {
        vault: OnChain {
            state: successor,
            outpoint: OutPoint::new(txid, 0),
            value: Sats::new(vault.value.raw().saturating_sub(intent.fee.raw())),
        },
        pot: OnChain {
            state: PotState,
            outpoint: OutPoint::new(txid, 1),
            value: Obol::new(protocol.pot.value.raw().saturating_sub(d)),
        },
        issuer: OnChain { state: issuer_succ, outpoint: OutPoint::new(txid, 3), value: 1 },
    };
    Built { plan, expected }
}
