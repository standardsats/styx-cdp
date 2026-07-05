//! Canonical accepting scenarios: one full transaction per vault op. Since M7 they are thin
//! wrappers over the checked builders (REDEEM included since M8), so the matrix runs against
//! the production layouts.
//!
//! Shared numbers: debt $50k (5M cents), owner key 10, last_height 100, collateral at 150%
//! of $120k/BTC (62.5M sats), pot 95M units, tick height 120. Liquidation scenarios use
//! their own price regimes (dip 63k, full-liq band 85k, crash 40k).

use styx_core::domain::{OnChain, PotState, VaultState};
use styx_core::elements::secp256k1_zkp as zkp;

use styx_core::encode::{Sig, VaultOp};

use styx_core::oracle::OracleTick;
use styx_core::units::{BlockHeight, Obol, Sats};

use super::{keypair, op_true_spk, protocol_state, synthetic_outpoint, TestDeploy, FEE};
use crate::build;
use crate::finalize::vault_sighash;
use crate::intent::{
    BadDebtIntent, CloseIntent, DrawIntent, FullLiqIntent, FundingCoin, LiquidateIntent, ObolCoin,
    RedeemIntent, RefreshIntent, RepayIntent,
};

use crate::plan::{SlotKind, TxPlan};

/// A canonical accepting plan plus the parameters cross-encodings are built from.
pub struct Scenario {
    pub plan: TxPlan,
    pub tick: OracleTick,
    /// The op's natural u64 payload: repay r, draw d, liquidate dd, redeem x; the debt for
    /// the ops without one.
    pub amount: Obol,
    pub owner: zkp::Keypair,
    pub vault: VaultState,
}

const DEBT: u64 = 5_000_000; // $50k in cents
const COLL: u64 = 62_500_000; // 150% at $120k/BTC
const POT: u64 = 95_000_000;
const LH: u32 = 100;
const H: u32 = 120;

fn base_vault(debt: u64) -> (VaultState, zkp::Keypair) {
    let owner = keypair(10);
    (
        VaultState {
            debt: Obol::new(debt),
            owner: owner.x_only_public_key().0,
            last_height: BlockHeight::new(LH),
        },
        owner,
    )
}

fn vault_on_chain(state: VaultState, coll: u64) -> OnChain<VaultState> {
    OnChain { state, outpoint: synthetic_outpoint(0xC0), value: Sats::new(coll) }
}

fn pot_on_chain() -> OnChain<PotState> {
    OnChain { state: PotState, outpoint: synthetic_outpoint(0xA0), value: Obol::new(POT) }
}

fn obol_coin(n: u8, value: u64) -> ObolCoin {
    ObolCoin { outpoint: synthetic_outpoint(n), value: Obol::new(value), spk: op_true_spk() }
}

fn fee_coin() -> FundingCoin {
    FundingCoin { outpoint: synthetic_outpoint(0xB3), value: Sats::new(1_000_000), spk: op_true_spk() }
}

/// Replace the vault slot's op (matrix cross-encoding, owner-sig filling).
pub fn set_vault_op(plan: &mut TxPlan, op: VaultOp) {
    for slot in &mut plan.slots {
        if let SlotKind::Vault { op: slot_op, .. } = &mut slot.kind {
            *slot_op = Box::new(op);
            return;
        }
    }
    panic!("no vault slot in plan");
}

/// Replace any slot's kind (issuer / stability matrices).
pub fn set_slot_kind(plan: &mut TxPlan, input: u32, kind: SlotKind) {
    for slot in &mut plan.slots {
        if slot.input == input {
            slot.kind = kind;
            return;
        }
    }
    panic!("no slot at input {input}");
}

pub fn owner_sig(_d: &TestDeploy, owner: &zkp::Keypair, digest: [u8; 32]) -> Sig {
    let msg = zkp::Message::from_digest(digest);
    Sig(*styx_core::secp().sign_schnorr_no_aux_rand(&msg, owner).as_ref())
}

fn signed(d: &TestDeploy, mut plan: TxPlan, owner: &zkp::Keypair) -> TxPlan {
    crate::sign::owner_sign(&d.ctx, &mut plan, owner).expect("sibling slots accept");
    plan
}

/// CLOSE: the payer covers the debt exactly; the freed collateral pays the fee.
pub fn close(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let intent = CloseIntent {
        payer: obol_coin(0xB2, DEBT),
        recipient_spk: op_true_spk(),
        payer_change_spk: op_true_spk(),
        fee: FEE,
    };
    let built = build::close::close(&d.ctx, &pot_on_chain(), &vault_on_chain(vault, COLL), &intent)
        .expect("builds");
    let plan = signed(d, built.plan, &owner);
    Scenario { plan, tick: d.tick(H, 120_000), amount: Obol::new(DEBT), owner, vault }
}

/// REPAY r = $20k: collateral preserved, fee from a separate coin.
pub fn repay(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let r = 2_000_000u64;
    let intent = RepayIntent {
        amount: Obol::new(r),
        payer: obol_coin(0xB2, r),
        payer_change_spk: op_true_spk(),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        fee: FEE,
    };
    let built = build::repay::repay(&d.ctx, &pot_on_chain(), &vault_on_chain(vault, COLL), &intent)
        .expect("builds");
    let plan = signed(d, built.plan, &owner);
    Scenario { plan, tick: d.tick(H, 120_000), amount: Obol::new(r), owner, vault }
}

/// DRAW d = $10k against a $30k vault: 150% re-checked post-draw.
pub fn draw(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(3_000_000);
    let tick = d.tick(H, 120_000);
    let intent = DrawIntent {
        amount: Obol::new(1_000_000),
        borrower_spk: op_true_spk(),
        tick: tick.clone(),
        fee: FEE,
    };
    let protocol = protocol_state(POT, 0, LH);
    let built =
        build::draw::draw(&d.ctx, &protocol, &vault_on_chain(vault, COLL), &intent).expect("builds");
    let plan = signed(d, built.plan, &owner);
    Scenario { plan, tick, amount: Obol::new(1_000_000), owner, vault }
}

/// REFRESH: permissionless ratchet advance, health proven at CR >= 130%.
pub fn refresh(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let tick = d.tick(H, 120_000);
    let intent =
        RefreshIntent { tick: tick.clone(), fee_coin: fee_coin(), change_spk: op_true_spk(), fee: FEE };
    let built = build::refresh::refresh(&d.ctx, &vault_on_chain(vault, COLL), &intent).expect("builds");
    Scenario { plan: built.plan, tick, amount: Obol::new(DEBT), owner, vault }
}

/// Partial LIQUIDATE in its native regime: 1 BTC collateral against $50k debt dips to
/// CR ~126% at $63k. The extraction cap (<= 1.15 x dd) bounds dd to ~35% of the debt at
/// this CR.
pub fn liquidate(d: &TestDeploy) -> Scenario {
    liquidate_with(d, d.tick(H, 63_000))
}

/// The liquidate layout under a caller-chosen tick. The amounts are sized for a max quote
/// of $63k: heal band [73_333_333, 76_111_111] sats, extraction cap 27_380_952 sats.
pub fn liquidate_with(d: &TestDeploy, tick: OracleTick) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let intent = liquidate_intent(tick.clone());
    let protocol = protocol_state(POT, 1_000_000, LH);
    let built =
        build::liquidate::liquidate(&d.ctx, &protocol, &vault_on_chain(vault, 100_000_000), &intent)
            .expect("builds");
    Scenario { plan: built.plan, tick, amount: intent.dd, owner, vault }
}

/// The canonical liquidate intent (dd $15k, residual 74M sats).
pub fn liquidate_intent(tick: OracleTick) -> LiquidateIntent {
    LiquidateIntent {
        dd: Obol::new(1_500_000),
        residual: Sats::new(74_000_000),
        keeper: obol_coin(0xB4, 1_500_000),
        keeper_spk: op_true_spk(),
        obol_change_spk: op_true_spk(),
        tick,
        fee: FEE,
    }
}

/// FULL-LIQ: CR ~106% at $85k; 1/3 of the excess to the reserve.
pub fn full_liq(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let tick = d.tick(H, 85_000);
    let intent = FullLiqIntent {
        keeper: obol_coin(0xB4, DEBT + 1_000),
        keeper_spk: op_true_spk(),
        obol_change_spk: op_true_spk(),
        tick: tick.clone(),
        fee: FEE,
    };
    let protocol = protocol_state(POT, 1_000_000, LH);
    let built = build::full_liq::full_liq(&d.ctx, &protocol, &vault_on_chain(vault, COLL), &intent)
        .expect("builds");
    Scenario { plan: built.plan, tick, amount: Obol::new(DEBT), owner, vault }
}

/// BAD-DEBT: CR ~50% at $40k; the 20% cap (M-1) binds against a 30M reserve.
pub fn bad_debt(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let tick = d.tick(H, 40_000);
    let intent = bad_debt_intent(tick.clone());
    let protocol = protocol_state(POT, 30_000_000, LH);
    let built = build::bad_debt::bad_debt(&d.ctx, &protocol, &vault_on_chain(vault, COLL), &intent)
        .expect("builds");
    Scenario { plan: built.plan, tick, amount: Obol::new(DEBT), owner, vault }
}

/// The canonical bad-debt intent (keeper covers the debt exactly, fee from a separate coin).
pub fn bad_debt_intent(tick: OracleTick) -> BadDebtIntent {
    BadDebtIntent {
        keeper: obol_coin(0xB4, DEBT),
        keeper_spk: op_true_spk(),
        obol_change_spk: op_true_spk(),
        fee_coin: fee_coin(),
        change_spk: op_true_spk(),
        tick,
        fee: FEE,
    }
}

/// REDEEM x at par backing: the peg-floor swap, 0.5% fee to the reserve.
pub fn redeem(d: &TestDeploy) -> Scenario {
    redeem_at(d, styx_core::units::RatioK::from_cr_percent(100))
}

/// REDEEM against an under-backed system (the E-2 tail): the quorum co-signs backing_k
/// below par, so the extraction is valued at the floor min(par, backing_k) while the 0.5%
/// fee stays par-priced (the covenant pins k = 1_000_000).
pub fn redeem_underbacked(d: &TestDeploy) -> Scenario {
    redeem_at(d, styx_core::units::RatioK::new(160_000_000)) // 80% backing
}

/// Kept for callers that need a specific op shape signed into a hand-built plan.
pub fn sign_owner_op(
    d: &TestDeploy,
    plan: &mut TxPlan,
    owner: &zkp::Keypair,
    op: impl Fn(Sig) -> VaultOp,
) {
    let digest = vault_sighash(&d.ctx, plan).expect("sibling slots accept");
    set_vault_op(plan, op(owner_sig(d, owner, digest)));
}
fn redeem_at(d: &TestDeploy, backing_k: styx_core::units::RatioK) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let x = 1_000_000u64;
    let tick = d.tick_bk(H, 120_000, backing_k);
    let intent = RedeemIntent {
        x: Obol::new(x),
        redeemer: obol_coin(0xB4, x),
        redeemer_spk: op_true_spk(),
        obol_change_spk: op_true_spk(),
        tick: tick.clone(),
        fee: FEE,
    };
    let protocol = protocol_state(POT, 1_000_000, LH);
    let built =
        build::redeem::redeem(&d.ctx, &protocol, &vault_on_chain(vault, COLL), &intent).expect("builds");
    Scenario { plan: built.plan, tick, amount: Obol::new(x), owner, vault }
}
