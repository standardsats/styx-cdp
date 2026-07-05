//! The on-node acceptance suite: the full protocol lifecycle on a Simplicity regtest
//! elementsd, plus negative spot-checks that calibrate "prune verdict == node verdict" for
//! the pinned simplicityhl rev (re-run these on any rev bump).
//!
//! Ignored by default: needs ELEMENTSD_EXE (set inside `nix develop`). Run with
//! `cargo test -p styx-node -- --ignored`.

use styx_core::consts::{K_FEE_HALF_PERCENT, K_OPEN_MIN};
use styx_core::domain::{OnChain, VaultState};
use styx_core::elements::confidential::Value as CValue;
use styx_core::elements::{OutPoint, Transaction};
use styx_core::math::coll_at_cr;
use styx_core::units::{Obol, Price, Sats};
use styx_node::client::{op_true, Node, FEE};
use styx_node::regtest::{deploy, Deployment, SUPPLY};
use styx_node::scan::scan_protocol;
use styx_node::BroadcastError;
use styx_pset::build;
use styx_pset::finalize::finalize;
use styx_pset::intent::{
    CloseIntent, DrawIntent, FullLiqIntent, FundingCoin, LiquidateIntent, ObolCoin, OpenIntent,
    PokeIntent, RedeemIntent, RefreshIntent, RepayIntent,
};
use styx_pset::plan::TxPlan;
use styx_pset::sign::owner_sign;

fn keypair(secret: u8) -> styx_core::elements::secp256k1_zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    styx_core::elements::secp256k1_zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

fn coin(outpoint: OutPoint, value: u64) -> FundingCoin {
    FundingCoin { outpoint, value: Sats::new(value), spk: op_true() }
}

fn obol(outpoint: OutPoint, value: u64) -> ObolCoin {
    ObolCoin { outpoint, value: Obol::new(value), spk: op_true() }
}

/// Broadcast the finalized plan and return the tx; the node must accept what the prune tier
/// accepted.
fn broadcast(dep: &Deployment, plan: &TxPlan) -> Transaction {
    let tx = finalize(&dep.ctx, plan).expect("prune accepts");
    if let Err(e) = dep.node.send_and_mine(&tx) {
        for (i, input) in tx.input.iter().enumerate() {
            let o = &input.previous_output;
            let out = dep
                .node
                .rpc(
                    "gettxout",
                    &[
                        elementsd::bitcoincore_rpc::jsonrpc::serde_json::json!(o.txid.to_string()),
                        elementsd::bitcoincore_rpc::jsonrpc::serde_json::json!(o.vout),
                    ],
                )
                .unwrap();
            eprintln!("input {i}: {o} -> {}", if out.is_null() { "MISSING" } else { "present" });
        }
        panic!("node rejected what the prune tier accepted: {e}");
    }
    tx
}

/// The calibration spot-check: a post-finalization tamper the prune tier would reject must
/// be rejected by the node too (the oracle signatures are amount-independent, so the witness
/// stays structurally valid and the covenant gate is the sole cause).
fn assert_node_rejects(node: &Node, tx: Transaction) {
    match node.send(&tx) {
        Err(BroadcastError::Rejected(_)) => {}
        other => panic!("the node must reject the tampered tx, got {other:?}"),
    }
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn full_lifecycle_smoke() {
    let owner = keypair(10);
    let owner_pk = owner.x_only_public_key().0;
    let dep = deploy(18_000_000);
    let ctx = &dep.ctx;
    let node = &dep.node;
    let mut protocol = dep.protocol;

    // Funding: three open coins sized exactly (collateral + 0.5% fee + tx fee) and four fee
    // coins. Vault A carries extra collateral so the dip lands in the partial band.
    let fee_a = coll_at_cr(5_000_000, Price::new(120_000), K_FEE_HALF_PERCENT).raw();
    let coll_a = 100_000_000u64;
    let coll_fc = coll_at_cr(5_000_000, Price::new(120_000), K_OPEN_MIN).raw(); // 62.5M
    let values = [
        coll_a + fee_a + FEE.raw(),
        coll_fc + fee_a + FEE.raw(),
        coll_fc + fee_a + FEE.raw(),
        1_000_000,
        1_000_000,
        1_000_000,
        1_000_000,
    ];
    let coins = node.fund_optrue(ctx.params.policy, &values).expect("funding");

    // POKE: advance the mint anchor to the tip.
    let h = node.height().unwrap();
    let built = build::poke::poke(
        ctx,
        &protocol.issuer,
        &PokeIntent {
            tick: dep.tick(h, 120_000),
            funding: coin(coins[3], 1_000_000),
            change_spk: op_true(),
            fee: FEE,
        },
    )
    .expect("builds");
    broadcast(&dep, &built.plan);
    protocol.issuer = built.expected.issuer;

    // OPEN A ($50k debt, 1 BTC collateral). Spot-check: a one-sat-short borrow fee is
    // rejected on node exactly as at the prune tier.
    let h = node.height().unwrap();
    let intent = OpenIntent {
        owner: owner_pk,
        principal: Obol::new(5_000_000),
        collateral: Sats::new(coll_a),
        borrower_spk: op_true(),
        funding: coin(coins[0], values[0]),
        tick: dep.tick(h, 120_000),
        fee: FEE,
    };
    let built = build::open::open(ctx, &protocol, &intent).expect("builds");
    let genuine = finalize(ctx, &built.plan).expect("prune accepts");
    let mut tampered = genuine.clone();
    let CValue::Explicit(v) = tampered.output[3].value else { panic!() };
    tampered.output[3].value = CValue::Explicit(v - 1);
    assert_node_rejects(node, tampered);
    node.send_and_mine(&genuine).expect("genuine open accepted");
    let mut vault_a = built.expected.vault;
    protocol.pot = built.expected.pot;
    protocol.reserve = built.expected.reserve;
    protocol.issuer = built.expected.issuer;
    let mut obol_a = OutPoint::new(genuine.txid(), 2); // 5M to the borrower

    // REPAY $20k: the payer overpays from the 5M coin, 3M returns as change.
    let intent = RepayIntent {
        amount: Obol::new(2_000_000),
        payer: obol(obol_a, 5_000_000),
        payer_change_spk: op_true(),
        fee_coin: coin(coins[4], 1_000_000),
        change_spk: op_true(),
        fee: FEE,
    };
    let mut built = build::repay::repay(ctx, &protocol.pot, &vault_a, &intent).expect("builds");
    owner_sign(ctx, &mut built.plan, &owner).expect("signs");
    let tx = broadcast(&dep, &built.plan);
    vault_a = built.expected.vault;
    protocol.pot = built.expected.pot;
    obol_a = OutPoint::new(tx.txid(), 2); // the 3M surplus

    // DRAW $10k more.
    let h = node.height().unwrap();
    let intent = DrawIntent {
        amount: Obol::new(1_000_000),
        borrower_spk: op_true(),
        tick: dep.tick(h, 120_000),
        fee: FEE,
    };
    let mut built = build::draw::draw(ctx, &protocol, &vault_a, &intent).expect("builds");
    owner_sign(ctx, &mut built.plan, &owner).expect("signs");
    let tx = broadcast(&dep, &built.plan);
    vault_a = built.expected.vault;
    protocol.pot = built.expected.pot;
    protocol.issuer = built.expected.issuer;
    let obol_draw = OutPoint::new(tx.txid(), 2); // 1M

    // REFRESH: a keeper advances the healthy vault's ratchet.
    let h = node.height().unwrap();
    let built = build::refresh::refresh(
        ctx,
        &vault_a,
        &RefreshIntent {
            tick: dep.tick(h, 120_000),
            fee_coin: coin(coins[5], 1_000_000),
            change_spk: op_true(),
            fee: FEE,
        },
    )
    .expect("builds");
    broadcast(&dep, &built.plan);
    vault_a = built.expected.vault;

    // Partial LIQUIDATE at the $50k dip (CR ~125%). Spot-check: a one-sat-short reserve
    // share is rejected on node.
    let h = node.height().unwrap();
    let intent = LiquidateIntent {
        dd: Obol::new(1_000_000),
        residual: Sats::new(80_000_000),
        keeper: obol(obol_draw, 1_000_000),
        keeper_spk: op_true(),
        obol_change_spk: op_true(),
        tick: dep.tick(h, 50_000),
        fee: FEE,
    };
    let built = build::liquidate::liquidate(ctx, &protocol, &vault_a, &intent).expect("builds");
    let genuine = finalize(ctx, &built.plan).expect("prune accepts");
    let mut tampered = genuine.clone();
    let CValue::Explicit(v) = tampered.output[3].value else { panic!() };
    tampered.output[3].value = CValue::Explicit(v - 1);
    assert_node_rejects(node, tampered);
    node.send_and_mine(&genuine).expect("genuine liquidate accepted");
    vault_a = built.expected.vault;
    protocol.pot = built.expected.pot;
    protocol.reserve = built.expected.reserve;

    // BAD-DEBT at the $35k crash (CR ~93%): the reserve covers shortfall + bounty.
    let h = node.height().unwrap();
    let intent = styx_pset::intent::BadDebtIntent {
        keeper: obol(obol_a, 3_000_000),
        keeper_spk: op_true(),
        obol_change_spk: op_true(),
        fee_coin: coin(coins[6], 1_000_000),
        change_spk: op_true(),
        tick: dep.tick(h, 35_000),
        fee: FEE,
    };
    let built = build::bad_debt::bad_debt(ctx, &protocol, &vault_a, &intent).expect("builds");
    broadcast(&dep, &built.plan);
    protocol.pot = built.expected.pot;
    protocol.reserve = built.expected.reserve;
    protocol.issuer = built.expected.issuer;

    // OPEN F and OPEN C at 150%, then merge their OBOL for the full-liq keeper coin.
    let open_at_150 = |funding: OutPoint,
                           value: u64,
                           protocol: &mut styx_core::domain::ProtocolState|
     -> (OnChain<VaultState>, OutPoint) {
        let h = node.height().unwrap();
        let intent = OpenIntent {
            owner: owner_pk,
            principal: Obol::new(5_000_000),
            collateral: Sats::new(coll_fc),
            borrower_spk: op_true(),
            funding: coin(funding, value),
            tick: dep.tick(h, 120_000),
            fee: FEE,
        };
        let built = build::open::open(ctx, protocol, &intent).expect("builds");
        let tx = broadcast(&dep, &built.plan);
        protocol.pot = built.expected.pot;
        protocol.reserve = built.expected.reserve;
        protocol.issuer = built.expected.issuer;
        (built.expected.vault, OutPoint::new(tx.txid(), 2))
    };
    let (vault_f, obol_f) = open_at_150(coins[1], values[1], &mut protocol);
    let (vault_c, obol_c) = open_at_150(coins[2], values[2], &mut protocol);
    let merged = node
        .merge_obol(ctx.params.policy, ctx.params.obol, &[(obol_f, 5_000_000), (obol_c, 5_000_000)])
        .expect("merge"); // 10M

    // FULL-LIQ F at $85k (CR ~106%): the keeper repays the debt, one third of the excess
    // goes to the reserve; the 5M OBOL change is the E-5 anchor.
    let h = node.height().unwrap();
    let intent = FullLiqIntent {
        keeper: obol(merged, 10_000_000),
        keeper_spk: op_true(),
        obol_change_spk: op_true(),
        tick: dep.tick(h, 85_000),
        fee: FEE,
    };
    let built = build::full_liq::full_liq(ctx, &protocol, &vault_f, &intent).expect("builds");
    let tx = broadcast(&dep, &built.plan);
    protocol.pot = built.expected.pot;
    protocol.reserve = built.expected.reserve;
    let obol_change = OutPoint::new(tx.txid(), 2); // 5M

    // REDEEM $10k against C at par. Spot-check: one extra sat to the redeemer is rejected.
    let h = node.height().unwrap();
    let intent = RedeemIntent {
        x: Obol::new(1_000_000),
        redeemer: obol(obol_change, 5_000_000),
        redeemer_spk: op_true(),
        obol_change_spk: op_true(),
        tick: dep.tick(h, 120_000),
        fee: FEE,
    };
    let built = build::redeem::redeem(ctx, &protocol, &vault_c, &intent).expect("builds");
    let genuine = finalize(ctx, &built.plan).expect("prune accepts");
    let mut tampered = genuine.clone();
    let CValue::Explicit(v0) = tampered.output[0].value else { panic!() };
    let CValue::Explicit(v2) = tampered.output[2].value else { panic!() };
    tampered.output[0].value = CValue::Explicit(v0 - 1);
    tampered.output[2].value = CValue::Explicit(v2 + 1);
    assert_node_rejects(node, tampered);
    node.send_and_mine(&genuine).expect("genuine redeem accepted");
    let vault_c = built.expected.vault;
    protocol.pot = built.expected.pot;
    protocol.reserve = built.expected.reserve;
    let obol_change = OutPoint::new(genuine.txid(), 4); // 4M redeemer change

    // CLOSE C: the payer covers the remaining $40k exactly, the collateral is freed.
    let intent = CloseIntent {
        payer: obol(obol_change, 4_000_000),
        recipient_spk: op_true(),
        payer_change_spk: op_true(),
        fee: FEE,
    };
    let mut built = build::close::close(ctx, &protocol.pot, &vault_c, &intent).expect("builds");
    owner_sign(ctx, &mut built.plan, &owner).expect("signs");
    broadcast(&dep, &built.plan);
    protocol.pot = built.expected.pot;

    // Every debt is repaid: the pot is back to the full supply, and the scanner agrees with
    // the tracked state (the calibration of scan_protocol against the delta bookkeeping).
    assert_eq!(protocol.pot.value, Obol::new(SUPPLY));
    let scanned = scan_protocol(node, ctx, protocol.issuer.state).expect("scan");
    assert_eq!(scanned.pot, protocol.pot);
    assert_eq!(scanned.reserve, protocol.reserve);
    assert_eq!(scanned.issuer, protocol.issuer);
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn scanner_refuses_a_fragmented_reserve_and_ignores_dust() {
    let dep = deploy(1_000_000);

    // Foreign-asset dust at the pot and issuer addresses: a griefer can always send it, and
    // the scanner's asset filter must keep the snapshot identical.
    dep.node
        .fund_address(dep.ctx.params.policy, &dep.ctx.artifacts.pot_spk(), 1_000)
        .expect("pot dust");
    dep.node
        .fund_address(
            dep.ctx.params.policy,
            &dep.ctx.artifacts.issuer_spk(&dep.protocol.issuer.state),
            1_000,
        )
        .expect("issuer dust");
    let scanned = scan_protocol(&dep.node, &dep.ctx, dep.protocol.issuer.state).expect("scan");
    assert_eq!(scanned, dep.protocol, "dust must not move the snapshot");

    // L-3: a second same-asset UTXO at the reserve's constant address must stop the builders.
    dep.node
        .fund_address(dep.ctx.params.policy, &dep.ctx.artifacts.stability_spk(), 500_000)
        .expect("fragment");
    match scan_protocol(&dep.node, &dep.ctx, dep.protocol.issuer.state) {
        Err(styx_node::ScanError::ReserveFragmented { count: 2 }) => {}
        other => panic!("expected ReserveFragmented, got {other:?}"),
    }
}

#[test]
#[ignore = "needs ELEMENTSD_EXE (run inside nix develop)"]
fn issuer_respend_is_classified_as_conflict() {
    // Two pokes built from the same issuer UTXO: the second broadcast is the singleton
    // contention mode and must classify as Conflict, not Rejected.
    let dep = deploy(1_000_000);
    let coins = dep.node.fund_optrue(dep.ctx.params.policy, &[1_000_000, 1_000_000]).expect("fees");
    let h = dep.node.height().unwrap();
    let poke = |funding: OutPoint| {
        build::poke::poke(
            &dep.ctx,
            &dep.protocol.issuer,
            &PokeIntent {
                tick: dep.tick(h, 120_000),
                funding: coin(funding, 1_000_000),
                change_spk: op_true(),
                fee: FEE,
            },
        )
        .expect("builds")
    };
    let first = finalize(&dep.ctx, &poke(coins[0]).plan).expect("prune accepts");
    let second = finalize(&dep.ctx, &poke(coins[1]).plan).expect("prune accepts");
    dep.node.send_and_mine(&first).expect("first poke lands");
    match dep.node.send(&second) {
        Err(BroadcastError::Conflict(_)) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
}
