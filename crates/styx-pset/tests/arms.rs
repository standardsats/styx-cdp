//! Arm-discrimination matrices: the encoding of op i against the canonical environment of
//! op j is accepted iff i == j. This is the direct proof that every encoder variant prunes
//! to its intended covenant arm - the failure mode the encoders exist to prevent is an
//! off-diagonal accept (a different arm runs).
//!
//! Golden digests of the pruned vault witness per variant pin the encoder + layout: they
//! churn only on intentional changes (the fixture is fully deterministic).

use styx_core::elements::hashes::{sha256, Hash};
use styx_core::encode::{IssuerOp, Sig, StabilityOp, VaultOp};
use styx_core::oracle::OracleTick;
use styx_core::params::xonly_u256;
use styx_core::units::{BlockHeight, Obol};
use styx_pset::build::open::open;
use styx_pset::build::poke::poke;
use styx_pset::finalize::{finalize, slot_witness, tx_before_slot, vault_sighash};
use styx_pset::plan::{SlotKind, TxPlan};
use styx_pset::testkit::scenarios::{self, owner_sig, set_slot_kind, set_vault_op, Scenario};
use styx_pset::testkit::{keypair, open_intent, poke_intent, protocol_state, TestDeploy};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The verdict of the slot at `input`, against the same transaction state `finalize` gives it
/// (all earlier slots witnessed).
fn slot_verdict_in_order(d: &TestDeploy, plan: &TxPlan, input: u32) -> bool {
    let tx = match tx_before_slot(&d.ctx, plan, input) {
        Ok(tx) => tx,
        Err(_) => return false, // an earlier slot already rejected
    };
    let slot = plan.slots.iter().find(|s| s.input == input).expect("slot exists");
    slot_witness(&d.ctx, &tx, &plan.in_utxos, slot).is_ok()
}

// --- vault: 8x8 ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum V {
    Close,
    Repay,
    Draw,
    Liquidate,
    FullLiq,
    BadDebt,
    Redeem,
    Refresh,
}
const VOPS: [V; 8] =
    [V::Close, V::Repay, V::Draw, V::Liquidate, V::FullLiq, V::BadDebt, V::Redeem, V::Refresh];

fn scenario(v: V) -> Scenario {
    let d = TestDeploy::get();
    match v {
        V::Close => scenarios::close(d),
        V::Repay => scenarios::repay(d),
        V::Draw => scenarios::draw(d),
        V::Liquidate => scenarios::liquidate(d),
        V::FullLiq => scenarios::full_liq(d),
        V::BadDebt => scenarios::bad_debt(d),
        V::Redeem => scenarios::redeem(d),
        V::Refresh => scenarios::refresh(d),
    }
}

/// The op-i encoding built from scenario-j parameters (its tick, its amount, a signature
/// over its vault sighash), so arm position is the only thing that varies.
fn vault_op_for(i: V, s: &Scenario, sig: Sig) -> VaultOp {
    let tick = s.tick.clone();
    match i {
        V::Close => VaultOp::Close { owner_sig: sig },
        V::Repay => VaultOp::Repay { owner_sig: sig, amount: s.amount },
        V::Draw => VaultOp::Draw { owner_sig: sig, amount: s.amount, tick },
        V::Liquidate => VaultOp::Liquidate { dd: s.amount, tick },
        V::FullLiq => VaultOp::FullLiq { tick },
        V::BadDebt => VaultOp::BadDebt { tick },
        V::Redeem => VaultOp::Redeem { x: s.amount, tick },
        V::Refresh => VaultOp::Refresh { tick },
    }
}

/// Off-diagonal cells that legitimately accept. Every cross-encoding carries a VALID owner
/// signature over the environment's sighash, and CLOSE is deliberately permissive: any tx
/// that returns the full debt to the pot is an authorized close once the owner has signed
/// it, wherever the freed collateral goes. The permissionless environments that repay the
/// full debt (full-liq, bad-debt) therefore satisfy the CLOSE arm under owner authorization.
/// No permissionless encoding ever accepts off-diagonal - that is the property the matrix
/// guards.
const ACCEPT_EXCEPTIONS: &[(V, V)] = &[(V::Close, V::FullLiq), (V::Close, V::BadDebt)];

#[test]
fn vault_matrix_is_diagonal() {
    let d = TestDeploy::get();
    let mut mismatches = Vec::new();
    for j in VOPS {
        let s = scenario(j);
        let digest = vault_sighash(&d.ctx, &s.plan).expect("sighash");
        for i in VOPS {
            let mut plan = s.plan.clone();
            set_vault_op(&mut plan, vault_op_for(i, &s, owner_sig(d, &s.owner, digest)));
            let accepted = slot_verdict_in_order(d, &plan, 0);
            let expected = i == j || ACCEPT_EXCEPTIONS.contains(&(i, j));
            if accepted != expected {
                mismatches.push(format!(
                    "encoding {i:?} against the {j:?} environment: expected {}, got {}",
                    if expected { "accept" } else { "reject" },
                    if accepted { "accept" } else { "reject" },
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "matrix mismatches:\n{}", mismatches.join("\n"));
}

// Recorded from the first green run; the fixture is deterministic (fixed keys, no_aux_rand,
// synthetic outpoints). sha256(witness_bytes || program_bytes) of the finalized vault input.
const VAULT_WITNESS_DIGESTS: [(&str, &str); 8] = [
    ("Close", "794fc5043082948a41d9f5aa000b9728697ba00a48b7980283ee0ea194d01bbc"),
    ("Repay", "e300251c5568115d7e159b31e02a1a668f3f8dd616986ad60b59bf4914f53dd2"),
    ("Draw", "5f27681099223986cb0595a72c846bb784c19c05020c1556c44e211c55058910"),
    ("Liquidate", "dff20bd67665a2846b4ca98763dfbc5e3385858d1d11cec2b5ec879a2f5d778c"),
    ("FullLiq", "bbfb10fc081c8b35b9d0570a91ea56b7037c8235e1fd0d0ad851c16f6d6b62db"),
    ("BadDebt", "d56480ee551a12773719e78ee62e15383ee693ea75fed3e4b89c710a04fcf03c"),
    ("Redeem", "eafe1e9b053dc3107f2d5de9d7d964d361f00fc3dd3907840f086a8c62a3ec95"),
    ("Refresh", "19b4319fd4447c1d2ff31d051a95fa331619d67db28fa49b93796779963d32bd"),
];

#[test]
fn vault_pruned_witness_digests_golden() {
    let d = TestDeploy::get();
    let mut mismatches = Vec::new();
    for (v, (name, expected)) in VOPS.iter().zip(VAULT_WITNESS_DIGESTS) {
        let s = scenario(*v);
        let tx = finalize(&d.ctx, &s.plan).expect("accepts");
        let w = &tx.input[0].witness.script_witness;
        let digest =
            hex(&sha256::Hash::hash(&[w[0].as_slice(), w[1].as_slice()].concat()).to_byte_array());
        if digest != expected {
            mismatches.push(format!("(\"{name}\", \"{digest}\"),"));
        }
    }
    assert!(mismatches.is_empty(), "digests changed:\n{}", mismatches.join("\n"));
}

// --- issuer: 4x4 ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum I {
    Open,
    Draw,
    Poke,
    Attest,
}
const IOPS: [I; 4] = [I::Open, I::Draw, I::Poke, I::Attest];

/// An issuer environment plus the amounts cross-encodings are built from. Like the vault
/// matrix, the off-diagonal ops reuse the environment's own numbers wherever the shapes
/// overlap, so arm position is what the cell tests - a rejection cannot be explained by an
/// amount mismatch the environment itself would produce.
struct IEnv {
    plan: TxPlan,
    input: u32,
    tick: OracleTick,
    /// The pot movement the environment performs (open principal / draw d); reused as the
    /// cross-encoded Open principal and Draw delta.
    moved: Obol,
    /// The vault identity the environment touches (input-0 vault where present).
    vault_debt: Obol,
    vault_lh: BlockHeight,
}

fn issuer_op_for(i: I, e: &IEnv) -> IssuerOp {
    let owner = xonly_u256(&keypair(10).x_only_public_key().0);
    match i {
        I::Open => IssuerOp::Open { principal: e.moved, owner, tick: e.tick.clone() },
        I::Draw => IssuerOp::Draw {
            old_debt: e.vault_debt,
            owner,
            old_last_height: e.vault_lh,
            new_debt: Obol::new(e.vault_debt.raw() + e.moved.raw()),
            draw_height: e.tick.height(),
        },
        I::Poke => IssuerOp::Poke { tick: e.tick.clone() },
        I::Attest => {
            IssuerOp::Attest { debt: e.vault_debt, owner, last_height: e.vault_lh, tick: e.tick.clone() }
        }
    }
}

fn issuer_env(j: I) -> IEnv {
    let d = TestDeploy::get();
    match j {
        I::Open => {
            let state = protocol_state(100_000_000, 1_000_000, 100);
            let tick = d.tick(120, 120_000);
            let built =
                open(&d.ctx, &state, &open_intent(tick.clone(), Obol::new(5_000_000))).expect("builds");
            // No input-0 vault exists; the vault fields describe the one being created.
            IEnv {
                plan: built.plan,
                input: 1,
                tick,
                moved: Obol::new(5_000_000),
                vault_debt: Obol::new(5_000_000),
                vault_lh: BlockHeight::new(120),
            }
        }
        I::Draw => {
            let s = scenarios::draw(d);
            IEnv {
                plan: s.plan,
                input: 2,
                tick: s.tick,
                moved: s.amount, // d = 1M
                vault_debt: s.vault.debt,
                vault_lh: s.vault.last_height,
            }
        }
        I::Poke => {
            let state = protocol_state(100_000_000, 0, 100);
            let tick = d.tick(120, 120_000);
            let built = poke(&d.ctx, &state.issuer, &poke_intent(tick.clone())).expect("builds");
            // No pot and no vault in a poke; canonical stand-ins.
            IEnv {
                plan: built.plan,
                input: 0,
                tick,
                moved: Obol::new(1_000_000),
                vault_debt: Obol::new(5_000_000),
                vault_lh: BlockHeight::new(100),
            }
        }
        I::Attest => {
            let s = scenarios::bad_debt(d);
            IEnv {
                plan: s.plan,
                input: 4,
                tick: s.tick,
                moved: s.vault.debt, // the pot grows by the full debt
                vault_debt: s.vault.debt,
                vault_lh: s.vault.last_height,
            }
        }
    }
}

#[test]
fn issuer_matrix_is_diagonal() {
    let d = TestDeploy::get();
    for j in IOPS {
        let e = issuer_env(j);
        for i in IOPS {
            let mut p = e.plan.clone();
            let state = match &e.plan.slots.iter().find(|s| s.input == e.input).expect("slot").kind {
                SlotKind::Issuer { state, .. } => *state,
                _ => panic!("not an issuer slot"),
            };
            // The diagonal keeps the environment's own op; off-diagonal ops are rebuilt from
            // the environment's numbers.
            if i != j {
                set_slot_kind(
                    &mut p,
                    e.input,
                    SlotKind::Issuer { state, op: Box::new(issuer_op_for(i, &e)) },
                );
            }
            let accepted = slot_verdict_in_order(d, &p, e.input);
            assert_eq!(accepted, i == j, "issuer encoding {i:?} against the {j:?} environment");
        }
    }
}

// --- directional-quorum and backing-floor scenarios ------------------------------

#[test]
fn liquidate_gates_at_the_max_quote() {
    // Diverging quotes with max $63k: the liquidate amounts are sized for a $63k valuation,
    // so acceptance proves the covenant prices the heal band at the MAX quote - at the $55k
    // min the band would demand a residual above 84M sats and reject.
    let d = TestDeploy::get();
    let s = scenarios::liquidate_with(
        d,
        styx_pset::testkit::tick_diverging(d, 120, [55_000, 63_000, 60_000]),
    );
    assert!(slot_verdict_in_order(d, &s.plan, 0), "liquidate under a diverging tick must accept");
    finalize(&d.ctx, &s.plan).expect("full plan accepts");
}

#[test]
fn redeem_underbacked_floors_the_extraction() {
    // backing_k = 80% of par, co-signed in the tick: the redeemer's extraction is valued at
    // the floor min(par, backing_k) - the E-2 tail. The scenario pays out 6_666_666 sats for
    // $10k of OBOL instead of the 8_333_333 par value, and the covenant accepts.
    let d = TestDeploy::get();
    let s = scenarios::redeem_underbacked(d);
    finalize(&d.ctx, &s.plan).expect("under-backed redeem accepts");
}

// --- stability: 2x2 -------------------------------------------------------------

#[test]
fn stability_matrix_is_diagonal() {
    let d = TestDeploy::get();
    // Accumulate environment: the OPEN tx (reserve grows by the borrow fee).
    let state = protocol_state(100_000_000, 1_000_000, 100);
    let acc = open(&d.ctx, &state, &open_intent(d.tick(120, 120_000), Obol::new(5_000_000)))
        .expect("builds")
        .plan;
    // BadDebt environment: the attested bad-debt close (reserve shrinks, issuer authors it).
    let bad = scenarios::bad_debt(d).plan;

    for (env_name, plan, diag) in
        [("accumulate", acc, StabilityOp::Accumulate), ("bad-debt", bad, StabilityOp::BadDebt)]
    {
        for op in [StabilityOp::Accumulate, StabilityOp::BadDebt] {
            let mut p = plan.clone();
            set_slot_kind(&mut p, 3, SlotKind::Stability(op));
            let accepted = slot_verdict_in_order(d, &p, 3);
            assert_eq!(accepted, op == diag, "stability {op:?} against the {env_name} environment");
        }
    }
}
