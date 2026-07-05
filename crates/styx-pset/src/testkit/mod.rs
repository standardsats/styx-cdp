//! Test fixtures for the prune tier and the e2e suite. Everything is deterministic: fixed
//! keys, synthetic outpoints, a constant genesis hash (the env and any sighash only need it
//! to be consistent, not real). Not for production use.

use std::sync::OnceLock;

use styx_core::artifacts::Artifacts;
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, ReserveState};
use styx_core::elements::hashes::Hash;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::{AssetId, BlockHash, OutPoint, Script, Transaction, Txid};
use styx_core::math::coll_at_cr;
use styx_core::oracle::{sign_quote, OracleSlot, OracleTick, TickPayload};
use styx_core::params::Params;
use styx_core::units::{BlockHeight, Obol, Price, RatioK, Sats};

use crate::error::PruneRejected;
use crate::finalize::{finalize, slot_witness};
use crate::intent::{FundingCoin, OpenIntent, PokeIntent};
use crate::plan::TxPlan;
use crate::Ctx;

pub const FEE: Sats = Sats::new(10_000);
pub const SUPPLY: Obol = Obol::new(100_000_000);

/// The fixed test deploy: dummy assets, fixed oracle secrets, a constant genesis hash. Compiled once per test binary.
pub struct TestDeploy {
    pub ctx: Ctx,
    pub oracle_keys: [zkp::Keypair; 5],
}

impl TestDeploy {
    pub fn get() -> &'static TestDeploy {
        static DEPLOY: OnceLock<TestDeploy> = OnceLock::new();
        DEPLOY.get_or_init(|| {
            let oracle_keys = [keypair(7), keypair(8), keypair(9), keypair(101), keypair(102)];
            let params = Params {
                obol: asset(0x01),
                issuer_token: asset(0x02),
                policy: asset(0x03),
                oracle_pks: oracle_keys.map(|k| k.x_only_public_key().0),
            };
            let artifacts = Artifacts::compile(&params).expect("test deploy compiles");
            TestDeploy {
                ctx: Ctx { params, artifacts, genesis: BlockHash::from_slice(&[0x42; 32]).unwrap() },
                oracle_keys,
            }
        })
    }

    /// A par-backed tick signed by oracles 1-3.
    pub fn tick(&self, height: u32, price: u32) -> OracleTick {
        self.tick_bk(height, price, RatioK::from_cr_percent(100))
    }

    pub fn tick_bk(&self, height: u32, price: u32, backing_k: RatioK) -> OracleTick {
        let payload =
            TickPayload { height: BlockHeight::new(height), price: Price::new(price), backing_k };
        let quotes = [0u8, 1, 2]
            .map(|i| (OracleSlot::new(i).unwrap(), sign_quote(&self.oracle_keys[i as usize], &payload)));
        OracleTick::new(payload.height, backing_k, quotes).unwrap()
    }
}

pub fn asset(byte: u8) -> AssetId {
    AssetId::from_slice(&[byte; 32]).unwrap()
}

pub fn keypair(secret: u8) -> zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

pub fn op_true_spk() -> Script {
    Script::from(vec![0x51])
}

/// A synthetic outpoint; the spend environment never dereferences it.
pub fn synthetic_outpoint(n: u8) -> OutPoint {
    OutPoint::new(Txid::from_slice(&[n; 32]).unwrap(), 0)
}

/// A protocol snapshot at synthetic outpoints.
pub fn protocol_state(pot_bal: u64, reserve_bal: u64, anchor: u32) -> ProtocolState {
    ProtocolState {
        pot: OnChain { state: PotState, outpoint: synthetic_outpoint(0xA0), value: Obol::new(pot_bal) },
        reserve: OnChain {
            state: ReserveState,
            outpoint: synthetic_outpoint(0xA1),
            value: Sats::new(reserve_bal),
        },
        issuer: OnChain {
            state: IssuerState { last_mint_height: BlockHeight::new(anchor) },
            outpoint: synthetic_outpoint(0xA2),
            value: 1,
        },
    }
}

/// A tick whose three quotes carry different prices - the directional quorum becomes visible
/// (min gates mints, max gates liquidations and redemptions). Each oracle signs its own
/// (height, price, backing_k).
pub fn tick_diverging(d: &TestDeploy, height: u32, prices: [u32; 3]) -> OracleTick {
    let quotes = [0u8, 1, 2].map(|i| {
        let payload = TickPayload {
            height: BlockHeight::new(height),
            price: Price::new(prices[i as usize]),
            backing_k: RatioK::from_cr_percent(100),
        };
        (OracleSlot::new(i).unwrap(), sign_quote(&d.oracle_keys[i as usize], &payload))
    });
    OracleTick::new(BlockHeight::new(height), RatioK::from_cr_percent(100), quotes).unwrap()
}

/// A poke intent funded by a synthetic op_true coin.
pub fn poke_intent(tick: OracleTick) -> PokeIntent {
    PokeIntent {
        tick,
        funding: FundingCoin {
            outpoint: synthetic_outpoint(0xB0),
            value: Sats::new(1_000_000),
            spk: op_true_spk(),
        },
        change_spk: op_true_spk(),
        fee: FEE,
    }
}

/// An open intent at exactly 150% CR with exact funding.
pub fn open_intent(tick: OracleTick, principal: Obol) -> OpenIntent {
    let (lo, _) = tick.price_range();
    let debt_cents = principal.covenant_cents().unwrap();
    let collateral = coll_at_cr(debt_cents, lo, styx_core::consts::K_OPEN_MIN);
    let borrow_fee = coll_at_cr(debt_cents, lo, styx_core::consts::K_FEE_HALF_PERCENT);
    let funding_value = Sats::new(collateral.raw() + borrow_fee.raw() + FEE.raw());
    OpenIntent {
        owner: keypair(10).x_only_public_key().0,
        principal,
        collateral,
        borrower_spk: op_true_spk(),
        funding: FundingCoin {
            outpoint: synthetic_outpoint(0xB1),
            value: funding_value,
            spk: op_true_spk(),
        },
        tick,
        fee: FEE,
    }
}

/// Finalize must succeed; returns the witnessed transaction.
#[track_caller]
pub fn assert_accepts(d: &TestDeploy, plan: &TxPlan) -> Transaction {
    finalize(&d.ctx, plan).unwrap_or_else(|e| panic!("expected accept, got: {e}"))
}

/// Finalize must reject at some covenant slot.
#[track_caller]
pub fn assert_rejects(d: &TestDeploy, plan: &TxPlan) -> PruneRejected {
    match finalize(&d.ctx, plan) {
        Ok(_) => panic!("expected a prune rejection, transaction was accepted"),
        Err(e) => e,
    }
}

/// The verdict of the single covenant slot at `input`, against the bare (unwitnessed)
/// transaction - the single-covenant probe pattern.
#[track_caller]
pub fn slot_verdict(d: &TestDeploy, plan: &TxPlan, input: u32) -> bool {
    let slot = plan
        .slots
        .iter()
        .find(|s| s.input == input)
        .unwrap_or_else(|| panic!("no covenant slot at input {input}"));
    slot_witness(&d.ctx, &plan.tx, &plan.in_utxos, slot).is_ok()
}

pub mod scenarios;
