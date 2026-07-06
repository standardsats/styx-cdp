//! The deployment ceremony against an already-running node with a loaded, funded wallet:
//! two non-reissuable issuances (OBOL into the pot, the 1-unit identity token into the
//! issuer at the current-height anchor), artifacts compiled against the real asset ids
//! (preflight runs inside), and the reserve seeded. Used by the regtest harness and by
//! styx-deploy against a live styxnet node.

use elementsd::bitcoincore_rpc::jsonrpc::serde_json::json;
use styx_core::artifacts::{Artifacts, DeployError};
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, ReserveState};
use styx_core::elements::hashes::Hash;
use styx_core::elements::hex::FromHex;
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::elements::{Address, AssetId, ContractHash, Transaction};
use styx_core::params::Params;
use styx_core::units::{BlockHeight, Obol, Sats};
use styx_pset::Ctx;

use crate::client::{out_array, Node, FEE_RPC};
use crate::{BroadcastError, NodeError};

/// The fixed OBOL supply the ceremony issues: 1M OBOL in atomic units.
pub const SUPPLY: u64 = 100_000_000;

#[derive(Debug, thiserror::Error)]
pub enum CeremonyError {
    #[error(transparent)]
    Node(#[from] NodeError),
    #[error(transparent)]
    Deploy(#[from] DeployError),
    #[error(transparent)]
    Broadcast(#[from] BroadcastError),
    #[error("unexpected rpc shape in {0}")]
    Shape(&'static str),
}

pub fn ceremony(
    node: &Node,
    oracle_pks: [XOnlyPublicKey; 5],
    reserve_seed: u64,
    confirm: &crate::client::Confirm,
) -> Result<(Ctx, ProtocolState), CeremonyError> {
    let genesis = node.genesis()?;
    let (_, _, policy) = node.biggest_coin()?;
    let (issue_in, issue_sats, token_in, token_sats) = node.split_two()?;
    let contract = ContractHash::from_byte_array([0u8; 32]);
    let obol = AssetId::new_issuance(issue_in, contract);
    let issuer_token = AssetId::new_issuance(token_in, contract);

    let params = Params { obol, issuer_token, policy, oracle_pks };
    let artifacts = Artifacts::compile(&params)?;
    let ctx = Ctx { params, artifacts, genesis };

    // Two issuances in one tx, both with no reissuance token: OBOL's supply is fixed
    // forever, and the issuer identity is a distinct fixed 1-unit asset with no minting
    // power.
    let ih0 = node.height()?;
    let issuer_state = IssuerState { last_mint_height: BlockHeight::new(ih0) };
    let net = &styx_core::elements::AddressParams::ELEMENTS;
    let pot_addr = Address::from_script(&ctx.artifacts.pot_spk(), None, net)
        .ok_or(CeremonyError::Shape("pot address"))?;
    let issuer_addr = Address::from_script(&ctx.artifacts.issuer_spk(&issuer_state), None, net)
        .ok_or(CeremonyError::Shape("issuer address"))?;
    let raw = node.rpc(
        "createrawtransaction",
        &[
            json!([{ "txid": issue_in.txid.to_string(), "vout": issue_in.vout },
                   { "txid": token_in.txid.to_string(), "vout": token_in.vout }]),
            out_array(&[
                ("fee".into(), FEE_RPC),
                (node.new_address()?.to_string(), issue_sats + token_sats - FEE_RPC),
            ]),
        ],
    )?;
    let detail = json!([
        { "asset_amount": SUPPLY as f64 / 1e8, "asset_address": pot_addr.to_string(),
          "blind": false, "contract_hash": "00".repeat(32) },
        { "asset_amount": 1.0 / 1e8, "asset_address": issuer_addr.to_string(),
          "blind": false, "contract_hash": "00".repeat(32) },
    ]);
    let issued = node.rpc("rawissueasset", &[raw, detail])?;
    let hex = issued
        .as_array()
        .and_then(|a| a.last())
        .and_then(|e| e["hex"].as_str())
        .ok_or(CeremonyError::Shape("rawissueasset hex"))?
        .to_string();
    let signed = node.rpc("signrawtransactionwithwallet", &[hex.into()])?;
    let bytes = signed["hex"]
        .as_str()
        .and_then(|h| Vec::<u8>::from_hex(h).ok())
        .ok_or(CeremonyError::Shape("issuance sign hex"))?;
    let tx: Transaction = styx_core::elements::encode::deserialize(&bytes)
        .map_err(|_| CeremonyError::Shape("issuance tx"))?;
    let txid = node.send(&tx)?;

    // The issuance must confirm before the reserve seed: the seed's coin selection only
    // sees confirmed change. On regtest / a private producer we mine the block ourselves;
    // on a federation chain we wait for one.
    let pot_op = node.find(txid, &ctx.artifacts.pot_spk(), SUPPLY)?;
    let issuer_op = node.find(txid, &ctx.artifacts.issuer_spk(&issuer_state), 1)?;
    node.confirm_outpoint(pot_op, confirm)?;
    let reserve_op = node.fund_address(policy, &ctx.artifacts.stability_spk(), reserve_seed)?;
    node.confirm_outpoint(reserve_op, confirm)?;

    let protocol = ProtocolState {
        pot: OnChain { state: PotState, outpoint: pot_op, value: Obol::new(SUPPLY) },
        reserve: OnChain { state: ReserveState, outpoint: reserve_op, value: Sats::new(reserve_seed) },
        issuer: OnChain { state: issuer_state, outpoint: issuer_op, value: 1 },
    };
    Ok((ctx, protocol))
}
