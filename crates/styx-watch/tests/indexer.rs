//! The indexer against a synthetic chain: builder-produced transaction bodies chained over
//! real outpoints, no node and no witnesses (layout inference must not need them). The
//! builder's `expected` delta is the ground truth at every step, and the builders take their
//! chain state back from the indexer - the loop the daemons will run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::consts::{K_FEE_HALF_PERCENT, K_OPEN_MIN};
use styx_core::domain::{IssuerState, OnChain, ProtocolState, VaultState};
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::elements::{OutPoint, Transaction};
use styx_core::math::coll_at_cr;
use styx_core::units::{BlockHeight, Obol, Price, Sats};
use styx_pset::build;
use styx_pset::intent::{
    BadDebtIntent, CloseIntent, DrawIntent, FullLiqIntent, FundingCoin, LiquidateIntent, ObolCoin,
    OpenIntent, PokeIntent, RedeemIntent, RefreshIntent, RepayIntent,
};
use styx_pset::layout::{fee_out, txin, txout};
use styx_pset::testkit::{keypair, op_true_spk, synthetic_outpoint, TestDeploy, FEE, SUPPLY};
use styx_watch::index::{Event, IndexState, Notice};

const SEED: u64 = 18_000_000;
const CEREMONY_H: u32 = 5;

fn coin(n: u8, value: u64) -> FundingCoin {
    FundingCoin { outpoint: synthetic_outpoint(n), value: Sats::new(value), spk: op_true_spk() }
}

fn obol(outpoint: OutPoint, value: u64) -> ObolCoin {
    ObolCoin { outpoint, value: Obol::new(value), spk: op_true_spk() }
}

fn owner_pk() -> XOnlyPublicKey {
    keypair(10).x_only_public_key().0
}

/// The fabricated ceremony: one issuance transaction (pot + issuer token) and the reserve
/// seed. The indexer must infer all three births.
fn boot(d: &TestDeploy, state: &mut IndexState) -> Vec<Notice> {
    let ctx = &d.ctx;
    let anchor = IssuerState { last_mint_height: BlockHeight::new(CEREMONY_H) };
    let issuance = Transaction {
        version: 2,
        lock_time: styx_core::elements::LockTime::ZERO,
        input: vec![txin(synthetic_outpoint(0xE0))],
        output: vec![
            txout(SUPPLY.raw(), ctx.artifacts.pot_spk(), ctx.params.obol),
            txout(1, ctx.artifacts.issuer_spk(&anchor), ctx.params.issuer_token),
            fee_out(FEE, ctx.params.policy),
        ],
    };
    let seed = Transaction {
        version: 2,
        lock_time: styx_core::elements::LockTime::ZERO,
        input: vec![txin(synthetic_outpoint(0xE1))],
        output: vec![
            txout(SEED, ctx.artifacts.stability_spk(), ctx.params.policy),
            fee_out(FEE, ctx.params.policy),
        ],
    };
    let mut notices = state.apply_tx(ctx, &[], CEREMONY_H, &issuance);
    notices.extend(state.apply_tx(ctx, &[], CEREMONY_H, &seed));
    notices
}

fn protocol(state: &IndexState) -> ProtocolState {
    state.protocol().expect("all three singletons live")
}

fn vault(state: &IndexState, op: OutPoint) -> OnChain<VaultState> {
    state.vaults.get(&op).expect("tracked").known(op).expect("owner resolved")
}

/// The exact OPEN funding: collateral + the 0.5% borrow fee + the tx fee.
fn open_funding(debt_cents: u32, price: u32, collateral: u64) -> u64 {
    collateral + coll_at_cr(debt_cents, Price::new(price), K_FEE_HALF_PERCENT).raw() + FEE.raw()
}

fn kind(n: &Notice) -> &Event {
    &n.event
}

#[test]
fn ceremony_births_all_three_singletons() {
    let d = TestDeploy::get();
    let mut state = IndexState::genesis(d.ctx.genesis);
    let notices = boot(d, &mut state);
    assert!(matches!(kind(&notices[0]), Event::PotBorn { value, .. } if *value == SUPPLY));
    assert!(matches!(
        kind(&notices[1]),
        Event::IssuerBorn { anchor, .. } if anchor.raw() == CEREMONY_H
    ));
    assert!(matches!(kind(&notices[2]), Event::ReserveBorn { value, .. } if value.raw() == SEED));
    let p = protocol(&state);
    assert_eq!(p.pot.value, SUPPLY);
    assert_eq!(p.reserve.value, Sats::new(SEED));
    assert_eq!(p.issuer.state.last_mint_height, BlockHeight::new(CEREMONY_H));
}

/// The e2e smoke's sequence, off node: every transition and removal inferred from layout,
/// each step checked against the builder's expected delta, the builders fed from the
/// indexer's own state.
#[test]
fn synthetic_lifecycle_matches_builder_deltas() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let owners = [owner_pk()];
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    // POKE at h=10.
    let built = build::poke::poke(
        ctx,
        &protocol(&state).issuer,
        &PokeIntent {
            tick: d.tick(10, 120_000),
            funding: coin(0xB0, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 10, &built.plan.tx);
    assert!(matches!(kind(&notices[0]), Event::Poked { anchor } if anchor.raw() == 10));
    assert_eq!(state.protocol().unwrap().issuer, built.expected.issuer);

    // OPEN A at h=12: $50k debt against 1 BTC.
    let coll_a = 100_000_000u64;
    let built = build::open::open(
        ctx,
        &protocol(&state),
        &OpenIntent {
            owner: owner_pk(),
            principal: Obol::new(5_000_000),
            collateral: Sats::new(coll_a),
            borrower_spk: op_true_spk(),
            funding: coin(0xB1, open_funding(5_000_000, 120_000, coll_a)),
            tick: d.tick(12, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let open_tx = built.plan.tx.clone();
    let notices = state.apply_tx(ctx, &owners, 12, &open_tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Opened { debt, owner: Some(_), .. } if debt.raw() == 5_000_000
    ));
    let mut a = built.expected.vault.outpoint;
    assert_eq!(vault(&state, a), built.expected.vault);
    assert_eq!(
        protocol(&state),
        ProtocolState {
            pot: built.expected.pot,
            reserve: built.expected.reserve,
            issuer: built.expected.issuer,
        }
    );
    let mut obol_a = OutPoint::new(open_tx.txid(), 2); // the 5M principal

    // REPAY $20k.
    let built = build::repay::repay(
        ctx,
        &protocol(&state).pot,
        &vault(&state, a),
        &RepayIntent {
            amount: Obol::new(2_000_000),
            payer: obol(obol_a, 5_000_000),
            payer_change_spk: op_true_spk(),
            fee_coin: coin(0xB2, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 13, &built.plan.tx);
    assert!(matches!(
        kind(&notices[0]),
        Event::Repaid { amount, .. } if amount.raw() == 2_000_000
    ));
    obol_a = OutPoint::new(built.plan.tx.txid(), 2); // the 3M surplus
    a = built.expected.vault.outpoint;
    assert_eq!(vault(&state, a), built.expected.vault);
    assert_eq!(protocol(&state).pot, built.expected.pot);

    // DRAW $10k at h=14.
    let built = build::draw::draw(
        ctx,
        &protocol(&state),
        &vault(&state, a),
        &DrawIntent {
            amount: Obol::new(1_000_000),
            borrower_spk: op_true_spk(),
            tick: d.tick(14, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 14, &built.plan.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Drawn { amount, .. } if amount.raw() == 1_000_000
    ));
    let obol_draw = OutPoint::new(built.plan.tx.txid(), 2); // 1M
    a = built.expected.vault.outpoint;
    assert_eq!(vault(&state, a), built.expected.vault);
    assert_eq!(protocol(&state).issuer, built.expected.issuer);

    // REFRESH at h=16.
    let built = build::refresh::refresh(
        ctx,
        &vault(&state, a),
        &RefreshIntent {
            tick: d.tick(16, 120_000),
            fee_coin: coin(0xB3, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 16, &built.plan.tx);
    assert!(matches!(
        kind(&notices[0]),
        Event::Refreshed { last_height, .. } if last_height.raw() == 16
    ));
    a = built.expected.vault.outpoint;
    assert_eq!(vault(&state, a), built.expected.vault);

    // Partial LIQUIDATE at the $50k dip.
    let built = build::liquidate::liquidate(
        ctx,
        &protocol(&state),
        &vault(&state, a),
        &LiquidateIntent {
            dd: Obol::new(1_000_000),
            residual: Sats::new(80_000_000),
            keeper: obol(obol_draw, 1_000_000),
            keeper_spk: op_true_spk(),
            obol_change_spk: op_true_spk(),
            tick: d.tick(18, 50_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 18, &built.plan.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Deleveraged { amount, .. } if amount.raw() == 1_000_000
    ));
    a = built.expected.vault.outpoint;
    assert_eq!(vault(&state, a), built.expected.vault);
    assert_eq!(protocol(&state).reserve, built.expected.reserve);

    // BAD-DEBT at the $35k crash: the vault ends, the reserve pays.
    let built = build::bad_debt::bad_debt(
        ctx,
        &protocol(&state),
        &vault(&state, a),
        &BadDebtIntent {
            keeper: obol(obol_a, 3_000_000),
            keeper_spk: op_true_spk(),
            obol_change_spk: op_true_spk(),
            fee_coin: coin(0xB4, 1_000_000),
            change_spk: op_true_spk(),
            tick: d.tick(20, 35_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 20, &built.plan.tx);
    assert!(notices.iter().any(|n| matches!(kind(n), Event::BadDebtClosed { vault } if *vault == a)));
    assert!(state.vaults.is_empty());
    assert_eq!(
        protocol(&state),
        ProtocolState {
            pot: built.expected.pot,
            reserve: built.expected.reserve,
            issuer: built.expected.issuer,
        }
    );

    // OPEN F and C at 150%, then FULL-LIQ F at $85k and REDEEM + CLOSE C.
    let coll_fc = coll_at_cr(5_000_000, Price::new(120_000), K_OPEN_MIN).raw();
    let mut open_at_150 = |h: u32, n: u8| -> (OutPoint, OnChain<VaultState>, OutPoint) {
        let built = build::open::open(
            ctx,
            &protocol(&state),
            &OpenIntent {
                owner: owner_pk(),
                principal: Obol::new(5_000_000),
                collateral: Sats::new(coll_fc),
                borrower_spk: op_true_spk(),
                funding: coin(n, open_funding(5_000_000, 120_000, coll_fc)),
                tick: d.tick(h, 120_000),
                fee: FEE,
            },
        )
        .expect("builds");
        state.apply_tx(ctx, &owners, h, &built.plan.tx);
        (built.expected.vault.outpoint, built.expected.vault, OutPoint::new(built.plan.tx.txid(), 2))
    };
    let (f, expected_f, obol_f) = open_at_150(22, 0xB5);
    let (c, expected_c, _obol_c) = open_at_150(24, 0xB6);
    assert_eq!(vault(&state, f), expected_f);
    assert_eq!(vault(&state, c), expected_c);

    // FULL-LIQ F at $85k: the keeper repays the full 5M; a 10M keeper coin leaves change.
    let built = build::full_liq::full_liq(
        ctx,
        &protocol(&state),
        &vault(&state, f),
        &FullLiqIntent {
            keeper: obol(obol_f, 10_000_000),
            keeper_spk: op_true_spk(),
            obol_change_spk: op_true_spk(),
            tick: d.tick(26, 85_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 26, &built.plan.tx);
    assert!(notices.iter().any(|n| matches!(kind(n), Event::FullyLiquidated { vault } if *vault == f)));
    assert!(!state.vaults.contains_key(&f));
    let obol_change = OutPoint::new(built.plan.tx.txid(), 2); // 5M OBOL change

    // REDEEM $10k against C.
    let built = build::redeem::redeem(
        ctx,
        &protocol(&state),
        &vault(&state, c),
        &RedeemIntent {
            x: Obol::new(1_000_000),
            redeemer: obol(obol_change, 5_000_000),
            redeemer_spk: op_true_spk(),
            obol_change_spk: op_true_spk(),
            tick: d.tick(28, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 28, &built.plan.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Deleveraged { amount, .. } if amount.raw() == 1_000_000
    ));
    let c = built.expected.vault.outpoint;
    assert_eq!(vault(&state, c), built.expected.vault);
    let obol_change = OutPoint::new(built.plan.tx.txid(), 4); // 4M redeemer change

    // CLOSE C: the pot returns to the full supply, no vault survives.
    let built = build::close::close(
        ctx,
        &protocol(&state).pot,
        &vault(&state, c),
        &CloseIntent {
            payer: obol(obol_change, 4_000_000),
            recipient_spk: op_true_spk(),
            payer_change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &owners, 30, &built.plan.tx);
    assert!(notices.iter().any(|n| matches!(kind(n), Event::Closed { vault } if *vault == c)));
    assert!(state.vaults.is_empty());
    assert!(state.lost.is_empty());
    assert_eq!(protocol(&state).pot.value, SUPPLY);
    assert_eq!(protocol(&state).pot, built.expected.pot);
}

/// A vault whose owner is not among the candidates: tracked opaque - same debt / value /
/// last_height bookkeeping off the layout, no builder-consumable state - and its transitions
/// still follow.
#[test]
fn foreign_owner_tracks_opaque() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let stranger = keypair(77);
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    // The e2e vault-A profile: $40k debt against 1 BTC, so the $50k dip lands in the
    // partial band with an in-band heal at residual 80M.
    let coll = 100_000_000u64;
    let built = build::open::open(
        ctx,
        &protocol(&state),
        &OpenIntent {
            owner: stranger.x_only_public_key().0,
            principal: Obol::new(4_000_000),
            collateral: Sats::new(coll),
            borrower_spk: op_true_spk(),
            funding: coin(0xC0, open_funding(4_000_000, 120_000, coll)),
            tick: d.tick(10, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    // The watcher only knows keypair(10); the stranger's vault must come out opaque.
    let notices = state.apply_tx(ctx, &[owner_pk()], 10, &built.plan.tx);
    assert!(matches!(kind(notices.last().unwrap()), Event::Opened { owner: None, .. }));
    let op = built.expected.vault.outpoint;
    let tracked = state.vaults.get(&op).expect("tracked");
    assert_eq!(tracked.owner, None);
    assert_eq!(tracked.debt, built.expected.vault.state.debt);
    assert_eq!(tracked.value, built.expected.vault.value);
    assert!(tracked.known(op).is_none(), "opaque vaults are not builder-consumable");

    // Its partial liquidation still tracks: debt and value follow the layout.
    let built = build::liquidate::liquidate(
        ctx,
        &protocol(&state),
        &built.expected.vault,
        &LiquidateIntent {
            dd: Obol::new(1_000_000),
            residual: Sats::new(80_000_000),
            keeper: obol(synthetic_outpoint(0xC1), 1_000_000),
            keeper_spk: op_true_spk(),
            obol_change_spk: op_true_spk(),
            tick: d.tick(12, 50_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let notices = state.apply_tx(ctx, &[owner_pk()], 12, &built.plan.tx);
    assert!(matches!(kind(notices.last().unwrap()), Event::Deleveraged { .. }));
    let op = built.expected.vault.outpoint;
    let tracked = state.vaults.get(&op).expect("tracked");
    assert_eq!(tracked.owner, None);
    assert_eq!(tracked.debt, built.expected.vault.state.debt);
    assert_eq!(tracked.value, built.expected.vault.value);
}

/// A spend of a known vault that fits no covenant layout marks it lost - both the
/// junk-layout case and the subtler one where the layout looks right but the successor spk
/// contradicts the transition.
#[test]
fn foreign_pattern_marks_the_vault_lost() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let owners = [owner_pk()];

    let open = |state: &mut IndexState, n: u8| -> OnChain<VaultState> {
        let coll = coll_at_cr(5_000_000, Price::new(120_000), K_OPEN_MIN).raw();
        let built = build::open::open(
            ctx,
            &protocol(state),
            &OpenIntent {
                owner: owner_pk(),
                principal: Obol::new(5_000_000),
                collateral: Sats::new(coll),
                borrower_spk: op_true_spk(),
                funding: coin(n, open_funding(5_000_000, 120_000, coll)),
                tick: d.tick(10, 120_000),
                fee: FEE,
            },
        )
        .expect("builds");
        state.apply_tx(ctx, &owners, 10, &built.plan.tx);
        built.expected.vault
    };

    // Junk layout: five inputs match no covenant arm.
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);
    let v = open(&mut state, 0xD0);
    let junk = Transaction {
        version: 2,
        lock_time: styx_core::elements::LockTime::ZERO,
        input: (0..5)
            .map(|i| if i == 0 { txin(v.outpoint) } else { txin(synthetic_outpoint(0xD1 + i)) })
            .collect(),
        output: vec![txout(1_000, op_true_spk(), ctx.params.policy)],
    };
    let notices = state.apply_tx(ctx, &owners, 12, &junk);
    assert!(matches!(kind(&notices[0]), Event::VaultLost { .. }));
    assert!(!state.vaults.contains_key(&v.outpoint));
    assert!(state.lost.contains_key(&v.outpoint));

    // REPAY-shaped layout whose output 0 is not the computed successor: the indexer must
    // not adopt a stranger's script as the vault.
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);
    let v = open(&mut state, 0xD8);
    let built = build::repay::repay(
        ctx,
        &protocol(&state).pot,
        &v,
        &RepayIntent {
            amount: Obol::new(2_000_000),
            payer: obol(synthetic_outpoint(0xD9), 2_000_000),
            payer_change_spk: op_true_spk(),
            fee_coin: coin(0xDA, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.output[0].script_pubkey = op_true_spk();
    });
    let notices = state.apply_tx(ctx, &owners, 12, &tampered.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::VaultLost { reason: "successor spk mismatch", .. }
    ));
    assert!(state.lost.contains_key(&v.outpoint));
}

/// Dust and donations at the constant addresses: a foreign asset never moves the snapshot
/// (the asset filter), and a same-asset payment that does not spend the singleton is a
/// fragment report, not a state change.
#[test]
fn dust_does_not_move_the_snapshot() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);
    let before = state.clone();

    let foreign = styx_pset::testkit::asset(0x66);
    let dust = Transaction {
        version: 2,
        lock_time: styx_core::elements::LockTime::ZERO,
        input: vec![txin(synthetic_outpoint(0xF0))],
        output: vec![
            txout(1_000, ctx.artifacts.pot_spk(), foreign),
            txout(1_000, ctx.artifacts.stability_spk(), foreign),
            txout(1_000, ctx.artifacts.issuer_spk(&before.issuer.unwrap().state), foreign),
            fee_out(FEE, ctx.params.policy),
        ],
    };
    let notices = state.apply_tx(ctx, &[], 10, &dust);
    assert!(notices.is_empty(), "foreign-asset dust must be invisible: {notices:?}");
    assert_eq!(state, before);

    // Same-asset fragments: reported, still no state movement.
    let fragment = Transaction {
        version: 2,
        lock_time: styx_core::elements::LockTime::ZERO,
        input: vec![txin(synthetic_outpoint(0xF1))],
        output: vec![
            txout(500_000, ctx.artifacts.stability_spk(), ctx.params.policy),
            fee_out(FEE, ctx.params.policy),
        ],
    };
    let notices = state.apply_tx(ctx, &[], 11, &fragment);
    assert!(matches!(kind(&notices[0]), Event::Fragment { role: "reserve" }));
    assert_eq!(state, before);
}

/// Save / load round trip, including an opaque vault and a lost record.
#[test]
fn snapshot_roundtrip_preserves_the_state() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    // One resolved vault, one opaque, one lost.
    let coll = coll_at_cr(5_000_000, Price::new(120_000), K_OPEN_MIN).raw();
    let mut open = |owner: XOnlyPublicKey, n: u8, h: u32| -> OutPoint {
        let built = build::open::open(
            ctx,
            &protocol(&state),
            &OpenIntent {
                owner,
                principal: Obol::new(5_000_000),
                collateral: Sats::new(coll),
                borrower_spk: op_true_spk(),
                funding: coin(n, open_funding(5_000_000, 120_000, coll)),
                tick: d.tick(h, 120_000),
                fee: FEE,
            },
        )
        .expect("builds");
        state.apply_tx(ctx, &[owner_pk()], h, &built.plan.tx);
        built.expected.vault.outpoint
    };
    open(owner_pk(), 0xA0, 10);
    let opaque = open(keypair(88).x_only_public_key().0, 0xA1, 12);
    let doomed = open(owner_pk(), 0xA2, 14);
    let junk = Transaction {
        version: 2,
        lock_time: styx_core::elements::LockTime::ZERO,
        input: vec![txin(doomed), txin(synthetic_outpoint(0xA3))],
        output: vec![txout(1_000, op_true_spk(), ctx.params.obol)],
    };
    // Two inputs shapes as REFRESH, but output 0 is not the successor: lost.
    state.apply_tx(ctx, &[owner_pk()], 16, &junk);
    state.seal(16, ctx.genesis);
    assert_eq!(state.vaults.len(), 2);
    assert!(state.vaults.get(&opaque).unwrap().owner.is_none());
    assert_eq!(state.lost.len(), 1);

    let dir = std::env::temp_dir().join(format!("styx-watch-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("snapshot.json");
    styx_watch::snapshot::save(&state, &path).unwrap();
    let loaded = styx_watch::snapshot::load(&path).unwrap();
    assert_eq!(loaded, state);
    // Idempotent re-save over the existing file (the atomic-replace path).
    styx_watch::snapshot::save(&loaded, &path).unwrap();
    assert_eq!(styx_watch::snapshot::load(&path).unwrap(), state);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// check_lock_height is only a LOWER bound on nLockTime: a covenant-valid op can carry a
/// locktime above the committed tick height. The indexer must recover the real anchor /
/// last_height by the spk scan, not lose the singleton or the owner resolution.
#[test]
fn inflated_locktime_keeps_the_singleton_and_the_owner() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let owners = [owner_pk()];
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    // POKE with tick height 10 but nLockTime 11.
    let built = build::poke::poke(
        ctx,
        &protocol(&state).issuer,
        &PokeIntent {
            tick: d.tick(10, 120_000),
            funding: coin(0xB0, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.lock_time = styx_core::elements::LockTime::from_consensus(11);
    });
    let notices = state.apply_tx(ctx, &owners, 12, &tampered.tx);
    assert!(matches!(kind(&notices[0]), Event::Poked { anchor } if anchor.raw() == 10));
    let issuer = state.protocol().expect("singleton survives").issuer;
    assert_eq!(issuer.state.last_mint_height, BlockHeight::new(10));
    assert_eq!(issuer.outpoint, OutPoint::new(tampered.tx.txid(), 0));

    // OPEN with tick height 12 but nLockTime 13: the anchor scan feeds the owner resolve.
    let built = build::open::open(
        ctx,
        &protocol(&state),
        &OpenIntent {
            owner: owner_pk(),
            principal: Obol::new(5_000_000),
            collateral: Sats::new(100_000_000),
            borrower_spk: op_true_spk(),
            funding: coin(0xB1, open_funding(5_000_000, 120_000, 100_000_000)),
            tick: d.tick(12, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.lock_time = styx_core::elements::LockTime::from_consensus(13);
    });
    let notices = state.apply_tx(ctx, &owners, 14, &tampered.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Opened { owner: Some(_), last_height, .. } if last_height.raw() == 12
    ));
    let vop = OutPoint::new(tampered.tx.txid(), 0);
    let tracked = state.vaults.get(&vop).expect("tracked");
    assert_eq!(tracked.owner, Some(owner_pk()));
    assert_eq!(tracked.last_height, BlockHeight::new(12));

    // REFRESH with tick height 16 but nLockTime 20: the ratchet scan recovers 16.
    let built = build::refresh::refresh(
        ctx,
        &vault(&state, vop),
        &RefreshIntent {
            tick: d.tick(16, 120_000),
            fee_coin: coin(0xB2, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.lock_time = styx_core::elements::LockTime::from_consensus(20);
    });
    let notices = state.apply_tx(ctx, &owners, 20, &tampered.tx);
    assert!(matches!(
        kind(&notices[0]),
        Event::Refreshed { last_height, .. } if last_height.raw() == 16
    ));
    let successor = OutPoint::new(tampered.tx.txid(), 0);
    assert_eq!(state.vaults.get(&successor).expect("tracked").last_height, BlockHeight::new(16));
}

/// Shapes the covenants cannot produce degrade to anomalies (or a lost vault) - never to a
/// false record.
#[test]
fn covenant_impossible_shapes_degrade_to_anomalies() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let owners = [owner_pk()];
    let open_intent = |n: u8, h: u32| OpenIntent {
        owner: owner_pk(),
        principal: Obol::new(5_000_000),
        collateral: Sats::new(100_000_000),
        borrower_spk: op_true_spk(),
        funding: coin(n, open_funding(5_000_000, 120_000, 100_000_000)),
        tick: d.tick(h, 120_000),
        fee: FEE,
    };

    // The token vanishes (successor spk unrecognizable): the issuer diverges to None.
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);
    let built = build::poke::poke(
        ctx,
        &protocol(&state).issuer,
        &PokeIntent {
            tick: d.tick(10, 120_000),
            funding: coin(0x90, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.output[0].script_pubkey = op_true_spk();
    });
    let notices = state.apply_tx(ctx, &owners, 10, &tampered.tx);
    assert!(matches!(
        kind(&notices[0]),
        Event::Anomaly { what: "issuer successor not found at any candidate anchor" }
    ));
    assert!(state.protocol().is_none());

    // OPEN-shaped with the reserve off its pinned input: anomaly, no vault recorded, the
    // issuer still followed.
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);
    let built = build::open::open(ctx, &protocol(&state), &open_intent(0x91, 10)).expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.input.swap(2, 3);
    });
    let notices = state.apply_tx(ctx, &owners, 10, &tampered.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Anomaly { what: "OPEN-shaped issuer spend without pot/reserve pins" }
    ));
    assert!(state.vaults.is_empty());
    assert_eq!(state.issuer.expect("followed").state.last_mint_height, BlockHeight::new(10));

    // A mint that does not shrink the pot: anomaly, no vault recorded.
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);
    let pot_before = protocol(&state).pot.value;
    let built = build::open::open(ctx, &protocol(&state), &open_intent(0x92, 10)).expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.output[1].value = styx_core::elements::confidential::Value::Explicit(pot_before.raw());
    });
    let notices = state.apply_tx(ctx, &owners, 10, &tampered.tx);
    assert!(notices
        .iter()
        .any(|n| matches!(n.event, Event::Anomaly { what: "mint op without a pot shrink" })));
    assert!(state.vaults.is_empty());

    // A repayment that does not grow the pot: anomaly, and the vault is lost rather than
    // mistracked.
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);
    let built = build::open::open(ctx, &protocol(&state), &open_intent(0x93, 10)).expect("builds");
    state.apply_tx(ctx, &owners, 10, &built.plan.tx);
    let pot_before = protocol(&state).pot.value;
    let v = built.expected.vault;
    let built = build::repay::repay(
        ctx,
        &protocol(&state).pot,
        &v,
        &RepayIntent {
            amount: Obol::new(2_000_000),
            payer: obol(OutPoint::new(built.plan.tx.txid(), 2), 5_000_000),
            payer_change_spk: op_true_spk(),
            fee_coin: coin(0x94, 1_000_000),
            change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.output[1].value = styx_core::elements::confidential::Value::Explicit(pot_before.raw());
    });
    let notices = state.apply_tx(ctx, &owners, 12, &tampered.tx);
    assert!(notices
        .iter()
        .any(|n| matches!(n.event, Event::Anomaly { what: "repayment op without a pot grow" })));
    assert!(state.lost.contains_key(&v.outpoint));
}

/// The one known mis-adoption margin, pinned: an opaque vault closed with a stray fourth
/// input reads as a full REPAY and leaves a debt-zero husk pointed at the recipient's coin.
/// Opaque-only (a resolved owner fails the successor-spk check into `lost`), worthless
/// either way (debt zero, not builder-consumable) - but the behavior is documented here.
#[test]
fn opaque_close_with_a_stray_input_pins_the_husk_margin() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    let coll = coll_at_cr(5_000_000, Price::new(120_000), K_OPEN_MIN).raw();
    let built = build::open::open(
        ctx,
        &protocol(&state),
        &OpenIntent {
            owner: keypair(99).x_only_public_key().0, // not a candidate: opaque
            principal: Obol::new(5_000_000),
            collateral: Sats::new(coll),
            borrower_spk: op_true_spk(),
            funding: coin(0x95, open_funding(5_000_000, 120_000, coll)),
            tick: d.tick(10, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let obol_op = OutPoint::new(built.plan.tx.txid(), 2);
    state.apply_tx(ctx, &[owner_pk()], 10, &built.plan.tx);
    let v = built.expected.vault;

    let built = build::close::close(
        ctx,
        &protocol(&state).pot,
        &v,
        &CloseIntent {
            payer: obol(obol_op, 5_000_000),
            recipient_spk: op_true_spk(),
            payer_change_spk: op_true_spk(),
            fee: FEE,
        },
    )
    .expect("builds");
    let tampered = built.plan.tamper(|tx, _| {
        tx.input.push(styx_pset::layout::txin(synthetic_outpoint(0x96)));
    });
    let notices = state.apply_tx(ctx, &[owner_pk()], 12, &tampered.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Repaid { amount, .. } if amount.raw() == 5_000_000
    ));
    let husk = state.vaults.get(&OutPoint::new(tampered.tx.txid(), 0)).expect("husk");
    assert_eq!(husk.debt, Obol::ZERO);
    assert_eq!(husk.owner, None);
}

/// A donation output smuggled into a protocol op is reported as a fragment; the pinned
/// successor (the first spk+asset match, which every earlier output slot cannot fake - the
/// other asset) still wins.
#[test]
fn a_donation_output_inside_an_op_reports_a_fragment() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let owners = [owner_pk()];
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    let built = build::open::open(
        ctx,
        &protocol(&state),
        &OpenIntent {
            owner: owner_pk(),
            principal: Obol::new(5_000_000),
            collateral: Sats::new(100_000_000),
            borrower_spk: op_true_spk(),
            funding: coin(0x97, open_funding(5_000_000, 120_000, 100_000_000)),
            tick: d.tick(10, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    let expected_pot = built.expected.pot.value;
    let tampered = built.plan.tamper(|tx, _| {
        tx.output.push(txout(1_000, ctx.artifacts.pot_spk(), ctx.params.obol));
    });
    let notices = state.apply_tx(ctx, &owners, 10, &tampered.tx);
    assert!(notices.iter().any(|n| matches!(n.event, Event::Fragment { role: "pot" })));
    assert!(notices.iter().any(|n| matches!(n.event, Event::Opened { .. })));
    let pot = state.protocol().expect("live").pot;
    assert_eq!(pot.value, expected_pot);
    assert_eq!(pot.outpoint, OutPoint::new(tampered.tx.txid(), 1));
}

/// The token's input position in OPEN is builder convention, not a covenant pin; the
/// dispatch keys on the covenant-pinned successor output (4) plus the covenant-read
/// relative positions (pot immediately before the token, reserve at 3), so a permuted
/// OPEN still classifies.
#[test]
fn open_with_the_token_off_the_conventional_input_still_classifies() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let owners = [owner_pk()];
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    let built = build::open::open(
        ctx,
        &protocol(&state),
        &OpenIntent {
            owner: owner_pk(),
            principal: Obol::new(5_000_000),
            collateral: Sats::new(100_000_000),
            borrower_spk: op_true_spk(),
            funding: coin(0x98, open_funding(5_000_000, 120_000, 100_000_000)),
            tick: d.tick(10, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    // [pot, issuer, funding, reserve] -> [funding, pot, issuer, reserve]: the token moves
    // to input 2 with the pot still immediately before it, as the covenant reads them.
    let tampered = built.plan.tamper(|tx, _| {
        let (pot, issuer, funding, reserve) =
            (tx.input[0].clone(), tx.input[1].clone(), tx.input[2].clone(), tx.input[3].clone());
        tx.input = vec![funding, pot, issuer, reserve];
    });
    let notices = state.apply_tx(ctx, &owners, 10, &tampered.tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Opened { owner: Some(_), debt, .. } if debt.raw() == 5_000_000
    ));
    let tracked = state.vaults.get(&OutPoint::new(tampered.tx.txid(), 0)).expect("tracked");
    assert_eq!(tracked.owner, Some(owner_pk()));
}

/// Candidate A of the R4 owner-recovery decision: with NO configured candidates, the owner
/// comes out of the OPEN's issuer-input witness via the sliding-window scan, verified
/// against the vault's address commitment - so a keeper tracks foreign vaults with full,
/// builder-consumable state.
#[test]
fn owner_recovered_from_the_open_witness_without_candidates() {
    let d = TestDeploy::get();
    let ctx = &d.ctx;
    let mut state = IndexState::genesis(ctx.genesis);
    boot(d, &mut state);

    let built = build::open::open(
        ctx,
        &protocol(&state),
        &OpenIntent {
            owner: owner_pk(),
            principal: Obol::new(5_000_000),
            collateral: Sats::new(100_000_000),
            borrower_spk: op_true_spk(),
            funding: coin(0x99, open_funding(5_000_000, 120_000, 100_000_000)),
            tick: d.tick(10, 120_000),
            fee: FEE,
        },
    )
    .expect("builds");
    // The finalized transaction carries the covenant witnesses (the owner travels in the
    // issuer input's witness values); the unwitnessed body would stay opaque.
    let tx = styx_pset::finalize::finalize(ctx, &built.plan).expect("prune accepts");
    let notices = state.apply_tx(ctx, &[], 10, &tx);
    assert!(matches!(
        kind(notices.last().unwrap()),
        Event::Opened { owner: Some(pk), .. } if *pk == owner_pk()
    ));
    let op = built.expected.vault.outpoint;
    assert_eq!(vault(&state, op), built.expected.vault, "builder-consumable without candidates");
}
