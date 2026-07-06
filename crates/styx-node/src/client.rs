//! A thin typed wrapper over the elementsd RPC, plus the wallet funding helpers the regtest
//! harness and the e2e suite use.

use std::str::FromStr;

use elementsd::bitcoincore_rpc::jsonrpc::serde_json::{json, Map, Value as JsonValue};
pub use elementsd::bitcoincore_rpc::Auth;
use elementsd::bitcoincore_rpc::{Client, RpcApi};
use elementsd::ElementsD;
use styx_core::elements::encode::{deserialize, serialize_hex};
use styx_core::elements::hex::FromHex;
use styx_core::elements::{Address, AssetId, Block, BlockHash, OutPoint, Script, Transaction, Txid};
use styx_core::units::Sats;

use crate::{BroadcastError, NodeError};

/// The standard tx fee the harness uses, matching the prune-tier fixtures.
pub const FEE: Sats = Sats::new(10_000);
/// The wallet-level fee for setup transactions.
pub const FEE_RPC: u64 = 100_000;

/// How a broadcast gets confirmed: by mining the block ourselves (regtest, a private
/// producer) or by waiting for whoever makes blocks on this chain (the testnet federation).
#[derive(Debug, Clone)]
pub enum Confirm {
    SelfMine,
    Await { timeout: std::time::Duration },
}

impl Confirm {
    /// A generous default for waiting chains: several testnet block intervals.
    pub fn await_default() -> Self {
        Confirm::Await { timeout: std::time::Duration::from_secs(300) }
    }
}

/// A connection to one elementsd. Daemons talk to their own machine's node by URL; the
/// regtest harness wraps the child process it spawned. The base URL and auth are kept so a
/// wallet-scoped client (`for_wallet`) can be derived once a wallet exists - wallet RPCs
/// need the `/wallet/<name>` path.
pub struct Node {
    base_url: String,
    auth: Auth,
    client: Client,
}

impl Node {
    /// Connect to an external node (the multi-machine setup: every daemon talks to its own
    /// elementsd, usually over localhost RPC).
    pub fn from_url(url: &str, auth: Auth) -> Result<Self, NodeError> {
        let client = Client::new(url, auth.clone())
            .map_err(|e| NodeError::Rpc { method: "connect".into(), message: e.to_string() })?;
        Ok(Node { base_url: url.trim_end_matches('/').to_string(), auth, client })
    }

    /// Connect to a harness-spawned child process (cookie auth).
    pub fn from_elementsd(d: &ElementsD) -> Result<Self, NodeError> {
        Self::from_url(&d.rpc_url(), Auth::CookieFile(d.params().cookie_file.clone()))
    }

    /// A client scoped to a loaded wallet (the `/wallet/<name>` URI path). Non-wallet RPCs
    /// still work through it.
    pub fn for_wallet(&self, wallet: &str) -> Result<Self, NodeError> {
        Self::from_url(&format!("{}/wallet/{wallet}", self.base_url), self.auth.clone())
    }

    pub fn rpc(&self, method: &str, args: &[JsonValue]) -> Result<JsonValue, NodeError> {
        self.client
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
        self.block_hash(0)
    }

    pub fn block_hash(&self, height: u32) -> Result<BlockHash, NodeError> {
        let s = self.rpc("getblockhash", &[height.into()])?;
        BlockHash::from_str(s.as_str().unwrap_or(""))
            .map_err(|_| NodeError::Shape { context: "getblockhash" })
    }

    /// The full block, witnesses included (the indexer walks these).
    pub fn block(&self, hash: BlockHash) -> Result<Block, NodeError> {
        let res = self.rpc("getblock", &[hash.to_string().into(), 0.into()])?;
        let hex = res.as_str().ok_or(NodeError::Shape { context: "getblock" })?;
        let bytes =
            Vec::<u8>::from_hex(hex).map_err(|_| NodeError::Shape { context: "getblock hex" })?;
        deserialize(&bytes).map_err(|_| NodeError::Shape { context: "getblock block" })
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

    /// Broadcast; contention surfaces as `Conflict` (the singleton contention mode:
    /// re-scan, rebuild, retry), everything else as `Rejected`. Broadcast is idempotent: a
    /// deterministic rebuild of a transaction the network already has (in the mempool or a
    /// block) is our own tx by txid, not a failure - daemons polling faster than blocks
    /// confirm hit this constantly.
    pub fn send(&self, tx: &Transaction) -> Result<Txid, BroadcastError> {
        match self.rpc("sendrawtransaction", &[serialize_hex(tx).into()]) {
            Ok(v) => Txid::from_str(v.as_str().unwrap_or(""))
                .map_err(|_| NodeError::Shape { context: "sendrawtransaction" }.into()),
            Err(NodeError::Rpc { message, .. }) => match classify_broadcast(&message) {
                BroadcastVerdict::Conflict => Err(BroadcastError::Conflict(message)),
                BroadcastVerdict::AlreadyKnown => Ok(tx.txid()),
                BroadcastVerdict::Rejected => Err(BroadcastError::Rejected(message)),
            },
            Err(e) => Err(e.into()),
        }
    }

    pub fn send_and_mine(&self, tx: &Transaction) -> Result<Txid, BroadcastError> {
        let txid = self.send(tx)?;
        self.mine()?;
        Ok(txid)
    }

    /// Wait until an outpoint is confirmed (visible to `gettxout` WITHOUT the mempool -
    /// the same visibility `scantxoutset` and the indexer have). `SelfMine` produces the
    /// block itself: regtest and private single-producer chains. `Await` polls: chains
    /// where someone else makes blocks (a producer daemon, the testnet federation).
    pub fn confirm_outpoint(&self, outpoint: OutPoint, mode: &Confirm) -> Result<(), NodeError> {
        let confirmed = |n: &Node| -> Result<bool, NodeError> {
            let out = n.rpc(
                "gettxout",
                &[json!(outpoint.txid.to_string()), json!(outpoint.vout), json!(false)],
            )?;
            Ok(!out.is_null())
        };
        match mode {
            Confirm::SelfMine => {
                self.mine()?;
                if confirmed(self)? {
                    Ok(())
                } else {
                    Err(NodeError::Shape { context: "confirm_outpoint: absent after self-mine" })
                }
            }
            Confirm::Await { timeout } => {
                let deadline = std::time::Instant::now() + *timeout;
                loop {
                    if confirmed(self)? {
                        return Ok(());
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(NodeError::Rpc {
                            method: "confirm_outpoint".into(),
                            message: format!("{outpoint} unconfirmed after {timeout:?}"),
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
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

    /// Sign with the node wallet and broadcast. No mining and no confirmation: spenders of
    /// the outputs may chain in the mempool; anything that needs confirmed visibility
    /// (scans, the indexer) calls `confirm_outpoint` with the chain-appropriate mode.
    fn sign_and_send_hex(&self, raw: JsonValue) -> Result<Txid, NodeError> {
        let signed = self.rpc("signrawtransactionwithwallet", &[raw])?;
        let hex = signed["hex"].as_str().ok_or(NodeError::Shape { context: "sign hex" })?;
        let bytes = Vec::<u8>::from_hex(hex).map_err(|_| NodeError::Shape { context: "sign hex" })?;
        let tx: Transaction =
            deserialize(&bytes).map_err(|_| NodeError::Shape { context: "sign tx" })?;
        match self.send(&tx) {
            Ok(t) => Ok(t),
            Err(e) => Err(NodeError::Rpc { method: "send".into(), message: e.to_string() }),
        }
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
    /// prune tier proves the real-key signing path, so the e2e can keep its coins keyless).
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

    /// Fund a specific scriptPubKey (seeding the reserve, the wallet on-ramp). The node
    /// wallet's spendable balance can read empty while the change of a just-broadcast
    /// transaction awaits a block (coin selection only sees confirmed coins); on a
    /// producing chain that resolves within a block, so wait for it, bounded.
    pub fn fund_address(
        &self,
        policy: AssetId,
        spk: &Script,
        value: u64,
    ) -> Result<OutPoint, NodeError> {
        use styx_pset::layout::{fee_out, txin, txout};
        let mut coin = self.biggest_coin();
        for _ in 0..120 {
            match &coin {
                Err(NodeError::Shape { context }) if *context == "listunspent empty" => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    coin = self.biggest_coin();
                }
                _ => break,
            }
        }
        let (op, sats, _) = coin?;
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

/// How a node's broadcast rejection reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastVerdict {
    /// Someone else's transaction holds our inputs (spent, in the mempool, or winning an
    /// RBF race): the contention mode the retry machinery resolves by resync + rebuild.
    Conflict,
    /// The network already has this exact transaction: idempotent success.
    AlreadyKnown,
    /// A genuine refusal (covenant, standardness): a builder invariant broke.
    Rejected,
}

/// Classify a sendrawtransaction error string. Everything here matches node error text and
/// is therefore calibrated against the pinned elementsd by the on-node e2e (the
/// conflict/idempotency test); re-run that suite on any node bump before trusting these
/// branches. The conflict class deliberately covers every same-inputs contention shape:
/// with several keepers racing on minute-long testnet blocks, mempool conflicts and lost
/// RBF races are the environment, not an anomaly - misreading them as Rejected would
/// alert-stop a healthy daemon.
pub fn classify_broadcast(message: &str) -> BroadcastVerdict {
    if message.contains("missingorspent")
        || message.contains("txn-mempool-conflict")
        || message.contains("insufficient fee, rejecting replacement")
        || message.contains("conflict")
    {
        BroadcastVerdict::Conflict
    } else if message.contains("already in block chain")
        || message.contains("already known")
        || message.contains("already-known")
        || message.contains("txn-already-in-mempool")
    {
        BroadcastVerdict::AlreadyKnown
    } else {
        BroadcastVerdict::Rejected
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
