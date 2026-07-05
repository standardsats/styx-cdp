//! A thin typed wrapper over the elementsd RPC, plus the wallet funding helpers the regtest
//! harness and the e2e suite use.

use std::str::FromStr;

use elementsd::bitcoincore_rpc::jsonrpc::serde_json::{json, Map, Value as JsonValue};
use elementsd::bitcoincore_rpc::RpcApi;
use elementsd::ElementsD;
use styx_core::elements::encode::{deserialize, serialize_hex};
use styx_core::elements::hex::FromHex;
use styx_core::elements::{Address, AssetId, BlockHash, OutPoint, Script, Transaction, Txid};
use styx_core::units::Sats;

use crate::{BroadcastError, NodeError};

/// The standard tx fee the harness uses, matching the prune-tier fixtures.
pub const FEE: Sats = Sats::new(10_000);
/// The wallet-level fee for setup transactions.
pub const FEE_RPC: u64 = 100_000;

pub struct Node {
    pub d: ElementsD,
}

impl Node {
    pub fn rpc(&self, method: &str, args: &[JsonValue]) -> Result<JsonValue, NodeError> {
        self.d
            .client()
            .call::<JsonValue>(method, args)
            .map_err(|e| NodeError::Rpc { method: method.into(), message: e.to_string() })
    }

    pub fn height(&self) -> Result<u32, NodeError> {
        self.rpc("getblockcount", &[])?
            .as_u64()
            .map(|h| h as u32)
            .ok_or(NodeError::Shape { context: "getblockcount" })
    }

    pub fn genesis(&self) -> Result<BlockHash, NodeError> {
        let s = self.rpc("getblockhash", &[0.into()])?;
        BlockHash::from_str(s.as_str().unwrap_or(""))
            .map_err(|_| NodeError::Shape { context: "getblockhash" })
    }

    pub fn mine(&self) -> Result<(), NodeError> {
        let a = self.new_address()?;
        self.rpc("generatetoaddress", &[1.into(), a.to_string().into()])?;
        Ok(())
    }

    pub fn new_address(&self) -> Result<Address, NodeError> {
        let s = self.rpc("getnewaddress", &[])?;
        Address::from_str(s.as_str().unwrap_or(""))
            .map_err(|_| NodeError::Shape { context: "getnewaddress" })
    }

    /// Broadcast; a missing-or-spent input surfaces as `Conflict` (the issuer-singleton
    /// contention mode: re-scan, rebuild, retry), everything else as `Rejected`.
    pub fn send(&self, tx: &Transaction) -> Result<Txid, BroadcastError> {
        match self.rpc("sendrawtransaction", &[serialize_hex(tx).into()]) {
            Ok(v) => Txid::from_str(v.as_str().unwrap_or(""))
                .map_err(|_| NodeError::Shape { context: "sendrawtransaction" }.into()),
            Err(NodeError::Rpc { message, .. }) => {
                if message.contains("missingorspent") || message.contains("conflict") {
                    Err(BroadcastError::Conflict(message))
                } else {
                    Err(BroadcastError::Rejected(message))
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn send_and_mine(&self, tx: &Transaction) -> Result<Txid, BroadcastError> {
        let txid = self.send(tx)?;
        self.mine()?;
        Ok(txid)
    }

    pub fn get_tx(&self, txid: Txid) -> Result<Transaction, NodeError> {
        let hex = self.rpc("gettransaction", &[txid.to_string().into()])?["hex"]
            .as_str()
            .ok_or(NodeError::Shape { context: "gettransaction" })?
            .to_string();
        let bytes =
            Vec::<u8>::from_hex(&hex).map_err(|_| NodeError::Shape { context: "gettransaction hex" })?;
        deserialize(&bytes).map_err(|_| NodeError::Shape { context: "gettransaction tx" })
    }

    /// The outpoint of the output paying `value` to `spk` in `txid`.
    pub fn find(&self, txid: Txid, spk: &Script, value: u64) -> Result<OutPoint, NodeError> {
        for (vout, out) in self.get_tx(txid)?.output.into_iter().enumerate() {
            if out.value == styx_core::elements::confidential::Value::Explicit(value)
                && &out.script_pubkey == spk
            {
                return Ok(OutPoint::new(txid, vout as u32));
            }
        }
        Err(NodeError::Shape { context: "find: output not present" })
    }

    pub fn biggest_coin(&self) -> Result<(OutPoint, u64, AssetId), NodeError> {
        let u = self.rpc("listunspent", &[])?;
        let list = u.as_array().ok_or(NodeError::Shape { context: "listunspent" })?;
        let c = list
            .iter()
            .max_by(|a, b| {
                let (x, y) = (a["amount"].as_f64().unwrap_or(0.0), b["amount"].as_f64().unwrap_or(0.0));
                x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or(NodeError::Shape { context: "listunspent empty" })?;
        let txid = Txid::from_str(c["txid"].as_str().unwrap_or(""))
            .map_err(|_| NodeError::Shape { context: "listunspent txid" })?;
        let asset = AssetId::from_str(c["asset"].as_str().unwrap_or(""))
            .map_err(|_| NodeError::Shape { context: "listunspent asset" })?;
        Ok((
            OutPoint::new(txid, c["vout"].as_u64().unwrap_or(0) as u32),
            (c["amount"].as_f64().unwrap_or(0.0) * 1e8).round() as u64,
            asset,
        ))
    }

    fn sign_and_send_hex(&self, raw: JsonValue) -> Result<Txid, NodeError> {
        let signed = self.rpc("signrawtransactionwithwallet", &[raw])?;
        let hex = signed["hex"].as_str().ok_or(NodeError::Shape { context: "sign hex" })?;
        let bytes = Vec::<u8>::from_hex(hex).map_err(|_| NodeError::Shape { context: "sign hex" })?;
        let tx: Transaction =
            deserialize(&bytes).map_err(|_| NodeError::Shape { context: "sign tx" })?;
        let txid = match self.send(&tx) {
            Ok(t) => t,
            Err(e) => return Err(NodeError::Rpc { method: "send".into(), message: e.to_string() }),
        };
        self.mine()?;
        Ok(txid)
    }

    /// Split the biggest wallet coin in two (the two issuance prevouts).
    pub fn split_two(&self) -> Result<(OutPoint, u64, OutPoint, u64), NodeError> {
        let (op, sats, _) = self.biggest_coin()?;
        let (a, b) = (self.new_address()?, self.new_address()?);
        let half = sats / 2;
        let rest = sats - half - FEE_RPC;
        let raw = self.rpc(
            "createrawtransaction",
            &[
                json!([{ "txid": op.txid.to_string(), "vout": op.vout }]),
                out_array(&[(a.to_string(), half), (b.to_string(), rest), ("fee".into(), FEE_RPC)]),
            ],
        )?;
        let txid = self.sign_and_send_hex(raw)?;
        Ok((
            self.find(txid, &a.script_pubkey(), half)?,
            half,
            self.find(txid, &b.script_pubkey(), rest)?,
            rest,
        ))
    }

    /// Fund op_true coins of the given values (funding and fee coins for the e2e flow; the
    /// prune tier proves the real-key signing path, the e2e keeps the prototype's shape).
    pub fn fund_optrue(&self, policy: AssetId, values: &[u64]) -> Result<Vec<OutPoint>, NodeError> {
        use styx_pset::layout::{fee_out, txin, txout};
        let (op, sats, _) = self.biggest_coin()?;
        let change = sats - (values.iter().sum::<u64>() + FEE.raw());
        let mut output: Vec<_> = values.iter().map(|v| txout(*v, op_true(), policy)).collect();
        output.push(txout(change, self.new_address()?.script_pubkey(), policy));
        output.push(fee_out(FEE, policy));
        let tx = Transaction {
            version: 2,
            lock_time: styx_core::elements::LockTime::ZERO,
            input: vec![txin(op)],
            output,
        };
        let txid = self.sign_and_send_hex(serialize_hex(&tx).into())?;
        // By position, not by value: several coins may share one value, and a value search
        // would return the same outpoint for all of them.
        Ok(values.iter().enumerate().map(|(i, _)| OutPoint::new(txid, i as u32)).collect())
    }

    /// Fund a specific scriptPubKey (seeding the reserve).
    pub fn fund_address(
        &self,
        policy: AssetId,
        spk: &Script,
        value: u64,
    ) -> Result<OutPoint, NodeError> {
        use styx_pset::layout::{fee_out, txin, txout};
        let (op, sats, _) = self.biggest_coin()?;
        let tx = Transaction {
            version: 2,
            lock_time: styx_core::elements::LockTime::ZERO,
            input: vec![txin(op)],
            output: vec![
                txout(value, spk.clone(), policy),
                txout(sats - value - FEE.raw(), self.new_address()?.script_pubkey(), policy),
                fee_out(FEE, policy),
            ],
        };
        let txid = self.sign_and_send_hex(serialize_hex(&tx).into())?;
        self.find(txid, spk, value)
    }

    /// Merge op_true OBOL UTXOs into one, the fee paid from a wallet coin.
    pub fn merge_obol(
        &self,
        policy: AssetId,
        obol: AssetId,
        parts: &[(OutPoint, u64)],
    ) -> Result<OutPoint, NodeError> {
        use styx_pset::layout::{fee_out, txin, txout};
        let total: u64 = parts.iter().map(|(_, v)| *v).sum();
        let (wc, wsats, _) = self.biggest_coin()?;
        let mut input = vec![txin(wc)];
        input.extend(parts.iter().map(|(o, _)| txin(*o)));
        let tx = Transaction {
            version: 2,
            lock_time: styx_core::elements::LockTime::ZERO,
            input,
            output: vec![
                txout(total, op_true(), obol),
                txout(wsats - FEE.raw(), self.new_address()?.script_pubkey(), policy),
                fee_out(FEE, policy),
            ],
        };
        let txid = self.sign_and_send_hex(serialize_hex(&tx).into())?;
        self.find(txid, &op_true(), total)
    }
}

pub fn op_true() -> Script {
    Script::from(vec![0x51])
}

pub fn out_array(pairs: &[(String, u64)]) -> JsonValue {
    JsonValue::Array(
        pairs
            .iter()
            .map(|(k, s)| {
                let mut m = Map::new();
                m.insert(k.clone(), json!(*s as f64 / 1e8));
                JsonValue::Object(m)
            })
            .collect(),
    )
}
