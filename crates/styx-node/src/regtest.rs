//! The regtest deployment ceremony: launch a Simplicity-capable elementsd, issue OBOL and
//! the 1-unit issuer identity token (both non-reissuable), compile the artifacts against the
//! real asset ids, and seed the reserve. Test-harness code: allowed to be opinionated.

use elementsd::bitcoincore_rpc::jsonrpc::serde_json::json;
use elementsd::ElementsD;
use styx_core::artifacts::Artifacts;
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, ReserveState};

use styx_core::elements::hashes::Hash;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::{AssetId, ContractHash};
use styx_core::units::{BlockHeight, Obol, Sats};
use styx_pset::Ctx;

use crate::client::{out_array, Node, FEE_RPC};

/// The fixed OBOL supply the ceremony issues: 1M OBOL in atomic units.
pub const SUPPLY: u64 = 100_000_000;

pub struct Deployment {
    pub node: Node,
    pub ctx: Ctx,
    pub protocol: ProtocolState,
    pub oracle_keys: [zkp::Keypair; 5],
}

fn keypair(secret: u8) -> zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    #[allow(clippy::unwrap_used)]
    zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

/// Launch the node and run the deployment ceremony. Panics on failure: this is the test
/// harness's entry point, not production deploy tooling.
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub fn deploy(reserve_seed: u64) -> Deployment {
    let mut conf = elementsd::Conf::new(None);
    let initial = "-initialfreecoins=210000000000";
    match conf.0.args.iter().position(|a| a.starts_with("-initialfreecoins=")) {
        Some(i) => conf.0.args[i] = initial,
        None => conf.0.args.push(initial),
    }
    conf.0.args.push("-evbparams=simplicity:-1:::");
    conf.0.args.push("-blindedaddresses=0");
    let d = ElementsD::with_conf(elementsd::exe_path().expect("set ELEMENTSD_EXE"), &conf)
        .expect("launch elementsd");
    let node = Node { d };
    node.rpc("createwallet", &["wallet".into()]).expect("createwallet");
    node.rpc("rescanblockchain", &[]).expect("rescan");
    let genesis = node.genesis().expect("genesis");

    // The policy asset (L-BTC) and the two issuance prevouts.
    let (_, _, policy) = {
        let (op, sats, asset) = node.biggest_coin().expect("coin");
        (op, sats, asset)
    };
    let (issue_in, issue_sats, token_in, token_sats) = node.split_two().expect("split");
    let contract = ContractHash::from_byte_array([0u8; 32]);
    let obol = AssetId::new_issuance(issue_in, contract);
    let issuer_token = AssetId::new_issuance(token_in, contract);

    // Compile the artifacts against the real asset ids; preflight runs inside.
    let oracle_keys = [keypair(7), keypair(8), keypair(9), keypair(101), keypair(102)];
    let params = styx_core::params::Params {
        obol,
        issuer_token,
        policy,
        oracle_pks: oracle_keys.map(|k| k.x_only_public_key().0),
    };
    let artifacts = Artifacts::compile(&params).expect("artifacts compile + preflight");
    let ctx = Ctx { params, artifacts, genesis };

    // Two issuances in one tx, both with no reissuance token: OBOL's supply is fixed forever
    // and the issuer identity is a distinct fixed 1-unit asset with no minting power.
    let ih0 = node.height().expect("height");
    let issuer_state = IssuerState { last_mint_height: BlockHeight::new(ih0) };
    let net = &styx_core::elements::AddressParams::ELEMENTS;
    let pot_addr = styx_core::elements::Address::from_script(&ctx.artifacts.pot_spk(), None, net)
        .expect("pot address");
    let issuer_addr =
        styx_core::elements::Address::from_script(&ctx.artifacts.issuer_spk(&issuer_state), None, net)
            .expect("issuer address");
    let raw = node
        .rpc(
            "createrawtransaction",
            &[
                json!([{ "txid": issue_in.txid.to_string(), "vout": issue_in.vout },
                       { "txid": token_in.txid.to_string(), "vout": token_in.vout }]),
                out_array(&[
                    ("fee".into(), FEE_RPC),
                    (node.new_address().expect("addr").to_string(), issue_sats + token_sats - FEE_RPC),
                ]),
            ],
        )
        .expect("createraw");
    let detail = json!([
        { "asset_amount": SUPPLY as f64 / 1e8, "asset_address": pot_addr.to_string(),
          "blind": false, "contract_hash": "00".repeat(32) },
        { "asset_amount": 1.0 / 1e8, "asset_address": issuer_addr.to_string(),
          "blind": false, "contract_hash": "00".repeat(32) },
    ]);
    let issued = node.rpc("rawissueasset", &[raw, detail]).expect("rawissueasset");
    let hex = issued.as_array().unwrap().last().unwrap()["hex"].clone();
    let signed = node.rpc("signrawtransactionwithwallet", &[hex]).expect("sign");
    let bytes =
        <Vec<u8> as styx_core::elements::hex::FromHex>::from_hex(signed["hex"].as_str().unwrap())
            .unwrap();
    let tx: styx_core::elements::Transaction = styx_core::elements::encode::deserialize(&bytes).unwrap();
    let txid = node.send_and_mine(&tx).expect("issuance broadcast");

    let pot_op = node.find(txid, &ctx.artifacts.pot_spk(), SUPPLY).expect("pot utxo");
    let issuer_op = node.find(txid, &ctx.artifacts.issuer_spk(&issuer_state), 1).expect("issuer utxo");
    let reserve_op =
        node.fund_address(policy, &ctx.artifacts.stability_spk(), reserve_seed).expect("reserve seed");

    let protocol = ProtocolState {
        pot: OnChain { state: PotState, outpoint: pot_op, value: Obol::new(SUPPLY) },
        reserve: OnChain { state: ReserveState, outpoint: reserve_op, value: Sats::new(reserve_seed) },
        issuer: OnChain { state: issuer_state, outpoint: issuer_op, value: 1 },
    };
    Deployment { node, ctx, protocol, oracle_keys }
}

impl Deployment {
    /// A par-backed tick at the given height and price, signed by oracles 1-3.
    pub fn tick(&self, height: u32, price: u32) -> styx_core::oracle::OracleTick {
        use styx_core::oracle::{sign_quote, OracleSlot, OracleTick, TickPayload};
        use styx_core::units::{Price, RatioK};
        let payload = TickPayload {
            height: BlockHeight::new(height),
            price: Price::new(price),
            backing_k: RatioK::from_cr_percent(100),
        };
        #[allow(clippy::unwrap_used)]
        let quotes = [0u8, 1, 2]
            .map(|i| (OracleSlot::new(i).unwrap(), sign_quote(&self.oracle_keys[i as usize], &payload)));
        #[allow(clippy::unwrap_used)]
        OracleTick::new(payload.height, payload.backing_k, quotes).unwrap()
    }
}
