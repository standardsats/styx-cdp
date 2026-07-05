//! POKE the issuer: advance the global mint-recency anchor to a newer tick, no token
//! movement. Permissionless; keepers run it on a schedule so the OPEN/DRAW freshness floor
//! tracks the tip.
//!
//! Layout (issuer.simf POKE arm; the token must sit at input 0 - the pot-drain guard):
//!   inputs  [issuer(0), fee coin(1)]
//!   outputs [issuer successor(0, token, advanced), change(1), fee]

use styx_core::domain::{IssuerState, OnChain};
use styx_core::elements::{LockTime, OutPoint, Transaction};
use styx_core::encode::IssuerOp;
use styx_core::units::Sats;

use crate::error::BuildError;
use crate::intent::PokeIntent;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{Built, SlotKind, TxPlan, WitnessSlot};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct PokeDelta {
    pub issuer: OnChain<IssuerState>,
}

pub fn poke(
    ctx: &Ctx,
    issuer: &OnChain<IssuerState>,
    intent: &PokeIntent,
) -> Result<Built<PokeDelta>, BuildError> {
    super::check_tick(&intent.tick, issuer.state.last_mint_height)?;
    // Strict: the change output must be positive, or the tx is dust-nonstandard.
    if intent.funding.value <= intent.fee {
        return Err(BuildError::InsufficientFunding { need: intent.fee, have: intent.funding.value });
    }
    Ok(poke_unchecked(ctx, issuer, intent))
}

/// The layout without the precondition checks; the covenant is the judge. Negative tests use
/// this to produce transactions whose only flaw is the gate under test.
pub fn poke_unchecked(
    ctx: &Ctx,
    issuer: &OnChain<IssuerState>,
    intent: &PokeIntent,
) -> Built<PokeDelta> {
    let a = &ctx.artifacts;
    let successor = IssuerState { last_mint_height: intent.tick.height() };
    let change = Sats::new(intent.funding.value.raw().saturating_sub(intent.fee.raw()));

    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(intent.tick.height().raw()),
        input: vec![txin(issuer.outpoint), txin(intent.funding.outpoint)],
        output: vec![
            txout(1, a.issuer_spk(&successor), ctx.params.issuer_token),
            txout(change.raw(), intent.change_spk.clone(), ctx.params.policy),
            fee_out(intent.fee, ctx.params.policy),
        ],
    };
    let in_utxos = vec![
        claimed(1, a.issuer_spk(&issuer.state), ctx.params.issuer_token),
        claimed(intent.funding.value.raw(), intent.funding.spk.clone(), ctx.params.policy),
    ];
    let slots = vec![WitnessSlot {
        input: 0,
        kind: SlotKind::Issuer {
            state: issuer.state,
            op: Box::new(IssuerOp::Poke { tick: intent.tick.clone() }),
        },
    }];

    let plan = TxPlan { tx, in_utxos, slots };
    let expected = PokeDelta {
        issuer: OnChain { state: successor, outpoint: OutPoint::new(plan.txid(), 0), value: 1 },
    };
    Built { plan, expected }
}
