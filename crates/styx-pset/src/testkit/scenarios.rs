//! Canonical accepting scenarios: one full transaction per vault op, the layouts the
//! covenants accept. The M5 discrimination matrices swap ops across these environments;
//! M6-M8 promote the layouts into checked builders with intents.
//!
//! Shared numbers: debt $50k (5M cents), owner key 10, last_height 100, collateral at 150%
//! of $120k/BTC (62.5M sats), pot 95M units, tick height 120. Liquidation scenarios use
//! their own price regimes (dip 63k, full-liq band 85k, crash 40k).

use styx_core::consts::{K_BAD_DEBT_CAP, K_FEE_HALF_PERCENT, K_PAR, K_RESERVE_SHARE};
use styx_core::domain::{IssuerState, VaultState};
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::{LockTime, Transaction};
use styx_core::encode::{IssuerOp, Sig, StabilityOp, VaultOp};
use styx_core::math::coll_at_cr;
use styx_core::oracle::OracleTick;
use styx_core::params::xonly_u256;
use styx_core::units::{BlockHeight, Obol, Price};

use super::{keypair, op_true_spk, synthetic_outpoint, TestDeploy, FEE};
use crate::finalize::vault_sighash;
use crate::layout::{claimed, fee_out, txin, txout};
use crate::plan::{SlotKind, TxPlan, WitnessSlot};

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

/// Sign the plan's vault sighash with the owner key and install the completed op.
pub fn sign_owner_op(d: &TestDeploy, plan: &mut TxPlan, owner: &zkp::Keypair, op: impl Fn(Sig) -> VaultOp) {
    let digest = vault_sighash(&d.ctx, plan).expect("sibling slots accept");
    let sig = owner_sig(d, owner, digest);
    set_vault_op(plan, op(sig));
}

pub fn owner_sig(_d: &TestDeploy, owner: &zkp::Keypair, digest: [u8; 32]) -> Sig {
    let msg = zkp::Message::from_digest(digest);
    Sig(*styx_core::secp().sign_schnorr_no_aux_rand(&msg, owner).as_ref())
}

fn vault_slot(state: VaultState, op: VaultOp) -> WitnessSlot {
    WitnessSlot { input: 0, kind: SlotKind::Vault { state, op: Box::new(op) } }
}

/// CLOSE: inputs [vault(0), pot(1), payer OBOL(2)]; outputs [freed collateral, pot + debt].
/// No oracle, no timelock; the tx fee comes from the freed collateral.
pub fn close(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::ZERO,
        input: vec![txin(synthetic_outpoint(0xC0)), txin(synthetic_outpoint(0xA0)), txin(synthetic_outpoint(0xB2))],
        output: vec![
            txout(COLL - FEE.raw(), op_true_spk(), p.policy),
            txout(POT + DEBT, a.pot_spk(), p.obol),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(DEBT, op_true_spk(), p.obol),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        vault_slot(vault, VaultOp::Close { owner_sig: Sig([0u8; 64]) }),
    ];
    let mut plan = TxPlan { tx, in_utxos, slots };
    sign_owner_op(d, &mut plan, &owner, |sig| VaultOp::Close { owner_sig: sig });
    Scenario { plan, tick: d.tick(H, 120_000), amount: Obol::new(DEBT), owner, vault }
}

/// REPAY r: collateral preserved in full; the tx fee comes from a separate coin.
pub fn repay(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let r = 2_000_000u64;
    let succ = VaultState { debt: Obol::new(DEBT - r), ..vault };
    let fee_coin = 1_000_000u64;
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::ZERO,
        input: vec![
            txin(synthetic_outpoint(0xC0)),
            txin(synthetic_outpoint(0xA0)),
            txin(synthetic_outpoint(0xB2)),
            txin(synthetic_outpoint(0xB3)),
        ],
        output: vec![
            txout(COLL, a.vault_spk(&succ), p.policy),
            txout(POT + r, a.pot_spk(), p.obol),
            txout(fee_coin - FEE.raw(), op_true_spk(), p.policy),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(r, op_true_spk(), p.obol),
        claimed(fee_coin, op_true_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        vault_slot(vault, VaultOp::Close { owner_sig: Sig([0u8; 64]) }),
    ];
    let mut plan = TxPlan { tx, in_utxos, slots };
    sign_owner_op(d, &mut plan, &owner, |sig| VaultOp::Repay { owner_sig: sig, amount: Obol::new(r) });
    Scenario { plan, tick: d.tick(H, 120_000), amount: Obol::new(r), owner, vault }
}

/// DRAW d: inputs [vault(0), pot(1), issuer(2)]; the tx fee comes from the collateral.
pub fn draw(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(3_000_000);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let dr = 1_000_000u64;
    let succ = VaultState {
        debt: Obol::new(3_000_000 + dr),
        owner: vault.owner,
        last_height: BlockHeight::new(H),
    };
    let issuer = IssuerState { last_mint_height: BlockHeight::new(LH) };
    let issuer_succ = IssuerState { last_mint_height: BlockHeight::new(H) };
    let tick = d.tick(H, 120_000);
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(H),
        input: vec![txin(synthetic_outpoint(0xC0)), txin(synthetic_outpoint(0xA0)), txin(synthetic_outpoint(0xA2))],
        output: vec![
            txout(COLL - FEE.raw(), a.vault_spk(&succ), p.policy),
            txout(POT - dr, a.pot_spk(), p.obol),
            txout(dr, op_true_spk(), p.obol),
            txout(1, a.issuer_spk(&issuer_succ), p.issuer_token),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(1, a.issuer_spk(&issuer), p.issuer_token),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotOutflow },
        WitnessSlot {
            input: 2,
            kind: SlotKind::Issuer {
                state: issuer,
                op: Box::new(IssuerOp::Draw {
                    old_debt: vault.debt,
                    owner: xonly_u256(&vault.owner),
                    old_last_height: vault.last_height,
                    new_debt: succ.debt,
                    draw_height: BlockHeight::new(H),
                }),
            },
        },
        vault_slot(vault, VaultOp::Close { owner_sig: Sig([0u8; 64]) }),
    ];
    let mut plan = TxPlan { tx, in_utxos, slots };
    let t = tick.clone();
    sign_owner_op(d, &mut plan, &owner, move |sig| VaultOp::Draw {
        owner_sig: sig,
        amount: Obol::new(dr),
        tick: t.clone(),
    });
    Scenario { plan, tick, amount: Obol::new(dr), owner, vault }
}

/// REFRESH: permissionless ratchet advance, health proven at CR >= 130%.
pub fn refresh(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let succ = VaultState { last_height: BlockHeight::new(H), ..vault };
    let fee_coin = 1_000_000u64;
    let tick = d.tick(H, 120_000);
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(H),
        input: vec![txin(synthetic_outpoint(0xC0)), txin(synthetic_outpoint(0xB3))],
        output: vec![
            txout(COLL, a.vault_spk(&succ), p.policy),
            txout(fee_coin - FEE.raw(), op_true_spk(), p.policy),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(fee_coin, op_true_spk(), p.policy),
    ];
    let slots = vec![vault_slot(vault, VaultOp::Refresh { tick: tick.clone() })];
    let plan = TxPlan { tx, in_utxos, slots };
    Scenario { plan, tick, amount: Obol::new(DEBT), owner, vault }
}

/// Partial LIQUIDATE in its native regime: 1 BTC collateral against $50k debt dips to
/// CR ~126% at $63k (below the 130% gate, above the 100% bad-debt line). The keeper repays
/// dd and heals the residual into [132%, 137%] at the max quote; the extraction cap
/// (<= 1.15 x dd) bounds dd to ~35% of the debt at this CR.
pub fn liquidate(d: &TestDeploy) -> Scenario {
    liquidate_with(d, d.tick(H, 63_000))
}

/// The liquidate layout under a caller-chosen tick. The amounts are sized for a max quote
/// of $63k: heal band [73_333_333, 76_111_111] sats, extraction cap 27_380_952 sats.
pub fn liquidate_with(d: &TestDeploy, tick: OracleTick) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let coll = 100_000_000u64; // this scenario's own collateral: CR ~126% at $63k
    let (_, hi) = tick.price_range();
    let (dd, residual, reserve_bal) = (1_500_000u64, 74_000_000u64, 1_000_000u64);
    let share = coll_at_cr(dd as u32, hi, K_RESERVE_SHARE).raw();
    let keeper_coll = coll - residual - share - FEE.raw();
    let succ = VaultState { debt: Obol::new(DEBT - dd), ..vault };
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(H),
        input: vec![
            txin(synthetic_outpoint(0xC0)),
            txin(synthetic_outpoint(0xA0)),
            txin(synthetic_outpoint(0xB4)),
            txin(synthetic_outpoint(0xA1)),
        ],
        output: vec![
            txout(residual, a.vault_spk(&succ), p.policy),
            txout(POT + dd, a.pot_spk(), p.obol),
            txout(keeper_coll, op_true_spk(), p.policy),
            txout(reserve_bal + share, a.stability_spk(), p.policy),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(coll, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(dd, op_true_spk(), p.obol),
        claimed(reserve_bal, a.stability_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::Accumulate) },
        vault_slot(vault, VaultOp::Liquidate { dd: Obol::new(dd), tick: tick.clone() }),
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    Scenario { plan, tick, amount: Obol::new(dd), owner, vault }
}

/// REDEEM against an under-backed system (the E-2 tail): the quorum co-signs
/// backing_k = 80% of par, so the redeemer's extraction is valued at the floor
/// min(par, backing_k) - 6_666_666 sats for $10k of OBOL at $120k/BTC instead of the
/// 8_333_333 par value. The 0.5% fee stays par-priced (the covenant pins k = 1_000_000).
pub fn redeem_underbacked(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let (price, x, reserve_bal) = (120_000u32, 1_000_000u64, 1_000_000u64);
    let backing_k = styx_core::units::RatioK::new(160_000_000); // 80% backing
    let x_worth = coll_at_cr(x as u32, Price::new(price), backing_k).raw();
    let fee_share = coll_at_cr(x as u32, Price::new(price), K_FEE_HALF_PERCENT).raw();
    let succ = VaultState { debt: Obol::new(DEBT - x), ..vault };
    let tick = d.tick_bk(H, price, backing_k);
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(H),
        input: vec![
            txin(synthetic_outpoint(0xC0)),
            txin(synthetic_outpoint(0xA0)),
            txin(synthetic_outpoint(0xB4)),
            txin(synthetic_outpoint(0xA1)),
        ],
        output: vec![
            txout(COLL - x_worth, a.vault_spk(&succ), p.policy),
            txout(POT + x, a.pot_spk(), p.obol),
            txout(x_worth - fee_share - FEE.raw(), op_true_spk(), p.policy),
            txout(reserve_bal + fee_share, a.stability_spk(), p.policy),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(x, op_true_spk(), p.obol),
        claimed(reserve_bal, a.stability_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::Accumulate) },
        vault_slot(vault, VaultOp::Redeem { x: Obol::new(x), tick: tick.clone() }),
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    Scenario { plan, tick, amount: Obol::new(x), owner, vault }
}

/// FULL-LIQ: CR in [100%, 115%] at $85k; 1/3 of the excess to the reserve.
pub fn full_liq(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let (price, reserve_bal) = (85_000u32, 1_000_000u64);
    let debt_sats = coll_at_cr(DEBT as u32, Price::new(price), K_PAR).raw();
    let reserve_fee = (COLL - debt_sats) / 3;
    let keeper_amt = DEBT + 1_000; // OBOL; the positive change is the E-5 anchor
    let tick = d.tick(H, price);
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(H),
        input: vec![
            txin(synthetic_outpoint(0xC0)),
            txin(synthetic_outpoint(0xA0)),
            txin(synthetic_outpoint(0xB4)),
            txin(synthetic_outpoint(0xA1)),
        ],
        output: vec![
            txout(COLL - reserve_fee - FEE.raw(), op_true_spk(), p.policy),
            txout(POT + DEBT, a.pot_spk(), p.obol),
            txout(keeper_amt - DEBT, op_true_spk(), p.obol),
            txout(reserve_bal + reserve_fee, a.stability_spk(), p.policy),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(keeper_amt, op_true_spk(), p.obol),
        claimed(reserve_bal, a.stability_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::Accumulate) },
        vault_slot(vault, VaultOp::FullLiq { tick: tick.clone() }),
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    Scenario { plan, tick, amount: Obol::new(DEBT), owner, vault }
}

/// BAD-DEBT: CR < 100% at $40k; issuer-attested, the reserve covers up to the 20% cap.
pub fn bad_debt(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let (price, reserve_bal, fee_coin) = (40_000u32, 30_000_000u64, 1_000_000u64);
    let debt_sats = coll_at_cr(DEBT as u32, Price::new(price), K_PAR).raw();
    let shortfall = debt_sats - COLL;
    let bounty = coll_at_cr(DEBT as u32, Price::new(price), K_RESERVE_SHARE).raw();
    let cap = coll_at_cr(DEBT as u32, Price::new(price), K_BAD_DEBT_CAP).raw();
    let reserve_pay = (shortfall + bounty).min(cap).min(reserve_bal);
    let issuer = IssuerState { last_mint_height: BlockHeight::new(LH) };
    let issuer_succ = IssuerState { last_mint_height: BlockHeight::new(H) };
    let tick = d.tick(H, price);
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(H),
        input: vec![
            txin(synthetic_outpoint(0xC0)),
            txin(synthetic_outpoint(0xA0)),
            txin(synthetic_outpoint(0xB4)),
            txin(synthetic_outpoint(0xA1)),
            txin(synthetic_outpoint(0xA2)),
            txin(synthetic_outpoint(0xB3)),
        ],
        output: vec![
            txout(COLL + reserve_pay, op_true_spk(), p.policy),
            txout(POT + DEBT, a.pot_spk(), p.obol),
            txout(reserve_bal - reserve_pay, a.stability_spk(), p.policy),
            txout(1, a.issuer_spk(&issuer_succ), p.issuer_token),
            txout(fee_coin - FEE.raw(), op_true_spk(), p.policy),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(DEBT, op_true_spk(), p.obol),
        claimed(reserve_bal, a.stability_spk(), p.policy),
        claimed(1, a.issuer_spk(&issuer), p.issuer_token),
        claimed(fee_coin, op_true_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::BadDebt) },
        WitnessSlot {
            input: 4,
            kind: SlotKind::Issuer {
                state: issuer,
                op: Box::new(IssuerOp::Attest {
                    debt: vault.debt,
                    owner: xonly_u256(&vault.owner),
                    last_height: vault.last_height,
                    tick: tick.clone(),
                }),
            },
        },
        vault_slot(vault, VaultOp::BadDebt { tick: tick.clone() }),
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    Scenario { plan, tick, amount: Obol::new(DEBT), owner, vault }
}

/// REDEEM x at par backing: the peg-floor swap, 0.5% fee to the reserve.
pub fn redeem(d: &TestDeploy) -> Scenario {
    let (vault, owner) = base_vault(DEBT);
    let a = &d.ctx.artifacts;
    let p = &d.ctx.params;
    let (price, x, reserve_bal) = (120_000u32, 1_000_000u64, 1_000_000u64);
    let x_worth = coll_at_cr(x as u32, Price::new(price), K_PAR).raw();
    let fee_share = coll_at_cr(x as u32, Price::new(price), K_FEE_HALF_PERCENT).raw();
    let succ = VaultState { debt: Obol::new(DEBT - x), ..vault };
    let tick = d.tick(H, price);
    let tx = Transaction {
        version: 2,
        lock_time: LockTime::from_consensus(H),
        input: vec![
            txin(synthetic_outpoint(0xC0)),
            txin(synthetic_outpoint(0xA0)),
            txin(synthetic_outpoint(0xB4)),
            txin(synthetic_outpoint(0xA1)),
        ],
        output: vec![
            txout(COLL - x_worth, a.vault_spk(&succ), p.policy),
            txout(POT + x, a.pot_spk(), p.obol),
            txout(x_worth - fee_share - FEE.raw(), op_true_spk(), p.policy),
            txout(reserve_bal + fee_share, a.stability_spk(), p.policy),
            fee_out(FEE, p.policy),
        ],
    };
    let in_utxos = vec![
        claimed(COLL, a.vault_spk(&vault), p.policy),
        claimed(POT, a.pot_spk(), p.obol),
        claimed(x, op_true_spk(), p.obol),
        claimed(reserve_bal, a.stability_spk(), p.policy),
    ];
    let slots = vec![
        WitnessSlot { input: 1, kind: SlotKind::PotInflow },
        WitnessSlot { input: 3, kind: SlotKind::Stability(StabilityOp::Accumulate) },
        vault_slot(vault, VaultOp::Redeem { x: Obol::new(x), tick: tick.clone() }),
    ];
    let plan = TxPlan { tx, in_utxos, slots };
    Scenario { plan, tick, amount: Obol::new(x), owner, vault }
}
