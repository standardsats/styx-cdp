//! Protocol-state scanning with the single-UTXO invariant (audit L-3): one pot, one reserve,
//! one issuer, or no `ProtocolState` at all - a builder must never see fragments.
//!
//! The pot and the reserve are constant addresses, so their UTXOs are discovered by script.
//! The issuer's address commits its mint anchor, so the scanner takes the expected state and
//! verifies the singleton exists there; recovering a lost anchor means deriving candidate
//! addresses over a height range (`find_issuer`). Foreign vaults with a non-point owner are a
//! chain fact the scanner must tolerate, which is why vault discovery is not part of this
//! snapshot: builders receive vault UTXOs from the caller's own tracking.

use elementsd::bitcoincore_rpc::jsonrpc::serde_json::json;
use styx_core::domain::{IssuerState, OnChain, PotState, ProtocolState, ReserveState};
use styx_core::elements::{OutPoint, Script, Txid};
use styx_core::units::{BlockHeight, Obol, Sats};
use styx_pset::Ctx;

use crate::client::Node;
use crate::ScanError;

/// The UTXOs of the expected asset at one scriptPubKey, via scantxoutset. The asset filter
/// is load-bearing: anyone can send dust of a foreign asset to the constant protocol
/// addresses, and an unfiltered scan would either report false fragmentation (pot, reserve)
/// or hand a builder a griefer's outpoint instead of the token (issuer) - the prune tier
/// would accept it against the claimed UTXO and the node would then reject.
fn scan_spk(
    node: &Node,
    spk: &Script,
    asset: styx_core::elements::AssetId,
) -> Result<Vec<(OutPoint, u64)>, ScanError> {
    use styx_core::elements::hex::ToHex;
    let desc = format!("raw({})", spk.as_bytes().to_hex());
    let res = node.rpc("scantxoutset", &[json!("start"), json!([{ "desc": desc }])])?;
    let unspents = res["unspents"]
        .as_array()
        .cloned()
        .ok_or(crate::NodeError::Shape { context: "scantxoutset unspents" })?;
    let mut out = Vec::new();
    for u in unspents {
        let utxo_asset: styx_core::elements::AssetId = u["asset"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(crate::NodeError::Shape { context: "scantxoutset asset" })?;
        if utxo_asset != asset {
            continue;
        }
        let txid: Txid = u["txid"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(crate::NodeError::Shape { context: "scantxoutset txid" })?;
        let vout =
            u["vout"].as_u64().ok_or(crate::NodeError::Shape { context: "scantxoutset vout" })? as u32;
        // scantxoutset reports BTC-denominated f64 amounts; exact for the protocol's range
        // (the 52-bit mantissa covers every representable satoshi amount).
        let amount =
            u["amount"].as_f64().ok_or(crate::NodeError::Shape { context: "scantxoutset amount" })?;
        out.push((OutPoint::new(txid, vout), (amount * 1e8).round() as u64));
    }
    Ok(out)
}

/// The protocol snapshot, refusing fragmented state.
pub fn scan_protocol(
    node: &Node,
    ctx: &Ctx,
    expected_issuer: IssuerState,
) -> Result<ProtocolState, ScanError> {
    let pots = scan_spk(node, &ctx.artifacts.pot_spk(), ctx.params.obol)?;
    if pots.len() > 1 {
        return Err(ScanError::PotFragmented { count: pots.len() });
    }
    let (pot_op, pot_val) = pots.into_iter().next().ok_or(ScanError::Missing { role: "pot" })?;

    let reserves = scan_spk(node, &ctx.artifacts.stability_spk(), ctx.params.policy)?;
    if reserves.len() > 1 {
        return Err(ScanError::ReserveFragmented { count: reserves.len() });
    }
    let (reserve_op, reserve_val) =
        reserves.into_iter().next().ok_or(ScanError::Missing { role: "reserve" })?;

    let issuers = scan_spk(node, &ctx.artifacts.issuer_spk(&expected_issuer), ctx.params.issuer_token)?;
    let (issuer_op, _) = issuers.into_iter().next().ok_or(ScanError::Missing { role: "issuer" })?;

    Ok(ProtocolState {
        pot: OnChain { state: PotState, outpoint: pot_op, value: Obol::new(pot_val) },
        reserve: OnChain { state: ReserveState, outpoint: reserve_op, value: Sats::new(reserve_val) },
        issuer: OnChain { state: expected_issuer, outpoint: issuer_op, value: 1 },
    })
}

/// Recover the issuer singleton by scanning candidate anchors over a height range. One full
/// UTXO-set scan per height: fine on regtest; a testnet recovery wants the batched variant
/// (one scantxoutset call with all the candidate descriptors).
pub fn find_issuer(
    node: &Node,
    ctx: &Ctx,
    heights: std::ops::RangeInclusive<u32>,
) -> Result<OnChain<IssuerState>, ScanError> {
    for h in heights {
        let state = IssuerState { last_mint_height: BlockHeight::new(h) };
        if let Some((outpoint, _)) =
            scan_spk(node, &ctx.artifacts.issuer_spk(&state), ctx.params.issuer_token)?
                .into_iter()
                .next()
        {
            return Ok(OnChain { state, outpoint, value: 1 });
        }
    }
    Err(ScanError::Missing { role: "issuer" })
}
