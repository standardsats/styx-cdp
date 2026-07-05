//! OPEN a vault: lock collateral, mint OBOL. The pot (input 0) releases the principal gated
//! by the issuer token (input 1); the issuer reconstructs the new vault and enforces the 150%
//! gate at the min quote plus the 0.5% borrow fee to the reserve (E-2).
//!
//! Layout (issuer.simf OPEN arm):
//!   inputs  [pot(0), issuer(1), funding(2), reserve(3)]
//!   outputs [vault(0), pot - principal(1), borrower principal(2), reserve + fee(3),
//!            issuer successor(4), fee]
//!
//! The funding coin covers collateral + borrow fee + tx fee exactly: the frozen layout has no
//! change output.

use styx_core::consts::{K_FEE_HALF_PERCENT, K_OPEN_MIN};
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, ReserveState, VaultState};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::{IssuerOp, StabilityOp};
use styx_core::math::coll_at_cr;
use styx_core::params::xonly_u256;
use styx_core::units::{Obol, Sats};

use crate::error::BuildError;
use crate::intent::OpenIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct OpenDelta {
    pub vault: OnChain<VaultState>,
    pub pot: OnChain<PotState>,
    pub reserve: OnChain<ReserveState>,
    pub issuer: OnChain<IssuerState>,
}

pub fn open(
    ctx: &Ctx,
    protocol: &ProtocolState,
    intent: &OpenIntent,
) -> Result<Built<OpenDelta>, BuildError> {
    super::check_tick(&intent.tick, protocol.issuer.state.last_mint_height)?;
    // A zero mint is forbidden by the issuer (issuer.simf:264) - it would open a pot drain
    // through the inflow leaf.
    if intent.principal == Obol::ZERO {
        return Err(BuildError::ZeroPrincipal);
    }
    let (lo, _) = intent.tick.price_range();
    let debt_cents = intent.principal.covenant_cents()?;
    // The issuer prices the 150% gate and the borrow fee at the min quote (issuer.simf:187,257).
    let need_coll = coll_at_cr(debt_cents, lo, K_OPEN_MIN);
    if intent.collateral < need_coll {
        return Err(BuildError::Undercollateralized { need: need_coll, have: intent.collateral });
    }
    if intent.principal > protocol.pot.value {
        return Err(BuildError::InsufficientPot {
            need: intent.principal,
            have: protocol.pot.value,
        });
    }
    let borrow_fee = coll_at_cr(debt_cents, lo, K_FEE_HALF_PERCENT);
    let need_funding = intent
        .collateral
        .checked_add(borrow_fee)?
        .checked_add(intent.fee)?;
    if intent.funding.value != need_funding {
        return Err(BuildError::FundingMismatch { need: need_funding, have: intent.funding.value });
    }
    Ok(open_unchecked(ctx, protocol, intent, borrow_fee))
}

/// The layout without the precondition checks; the covenant is the judge.
pub fn open_unchecked(
    ctx: &Ctx,
    protocol: &ProtocolState,
    intent: &OpenIntent,
    borrow_fee: Sats,
) -> Built<OpenDelta> {
    let a = &ctx.artifacts;
    let p = &ctx.params;
    let vault = VaultState {
        debt: intent.principal,
        owner: intent.owner,
        last_height: intent.tick.height(),
    };
    let issuer_successor = IssuerState { last_mint_height: intent.tick.height() };
    let pot_out = protocol.pot.value.raw().saturating_sub(intent.principal.raw());
    let reserve_out = protocol.reserve.value.raw().saturating_add(borrow_fee.raw());

    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(intent.tick.height().raw()),
        input: vec![
            txin(protocol.pot.outpoint),
            txin(protocol.issuer.outpoint),
            txin(intent.funding.outpoint),
            txin(protocol.reserve.outpoint),
        ],
        output: vec![
            txout(intent.collateral.raw(), a.vault_spk(&vault), p.policy),
            txout(pot_out, a.pot_spk(), p.obol),
            txout(intent.principal.raw(), intent.borrower_spk.clone(), p.obol),
            txout(reserve_out, a.stability_spk(), p.policy),
            txout(1, a.issuer_spk(&issuer_successor), p.issuer_token),
            fee_out(intent.fee, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(protocol.pot.value.raw(), a.pot_spk(), p.obol),
        claimed(1, a.issuer_spk(&protocol.issuer.state), p.issuer_token),
        claimed(intent.funding.value.raw(), intent.funding.spk.clone(), p.policy),
        claimed(protocol.reserve.value.raw(), a.stability_spk(), p.policy),
    ];
    // Witnessing order mirrors the prototype: pot outflow, reserve accumulate, then the
    // issuer, which reads both.
    let slots = vec![
        WitnessSlot { input: 0, kind: SlotKind::PotOutflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::Accumulate) },
        WitnessSlot {
            input: 1,
            kind: SlotKind::Issuer {
                state: protocol.issuer.state,
                op: IssuerOp::Open {
                    principal: intent.principal,
                    owner: xonly_u256(&intent.owner),
                    tick: intent.tick.clone(),
                },
            },
        },
    ];

    let plan = TxPlan { tx, in_utxos, slots };
    let txid = plan.txid();
    let expected = OpenDelta {
        vault: OnChain { state: vault, outpoint: OutPoint::new(txid, 0), value: intent.collateral },
        pot: OnChain { state: PotState, outpoint: OutPoint::new(txid, 1), value: Obol::new(pot_out) },
        reserve: OnChain {
            state: ReserveState,
            outpoint: OutPoint::new(txid, 3),
            value: Sats::new(reserve_out),
        },
        issuer: OnChain { state: issuer_successor, outpoint: OutPoint::new(txid, 4), value: 1 },
    };
    Built { plan, expected }
}
