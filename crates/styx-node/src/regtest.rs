//! The regtest harness: launch a Simplicity-capable elementsd child and run the deployment
//! ceremony against it with the fixed test oracle keys. Test-harness code: panics on
//! failure by design.

use elementsd::ElementsD;
use styx_core::domain::ProtocolState;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::units::BlockHeight;
use styx_pset::Ctx;

use crate::ceremony::ceremony;
pub use crate::ceremony::SUPPLY;
use crate::client::Node;

pub struct Deployment {
    /// The child process; dropping it stops the node.
    pub daemon: ElementsD,
    pub node: Node,
    pub ctx: Ctx,
    pub protocol: ProtocolState,
    pub oracle_keys: [zkp::Keypair; 5],
}

fn keypair(secret: u8) -> zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

/// Launch a node with `initialfreecoins` genesis coins and a loaded, rescanned wallet.
fn launch(initialfreecoins: u64) -> (ElementsD, Node) {
    let mut conf = elementsd::Conf::new(None);
    let initial: &'static str =
        Box::leak(format!("-initialfreecoins={initialfreecoins}").into_boxed_str());
    match conf.0.args.iter().position(|a| a.starts_with("-initialfreecoins=")) {
        Some(i) => conf.0.args[i] = initial,
        None => conf.0.args.push(initial),
    }
    conf.0.args.push("-evbparams=simplicity:-1:::");
    conf.0.args.push("-blindedaddresses=0");
    let daemon = ElementsD::with_conf(elementsd::exe_path().expect("set ELEMENTSD_EXE"), &conf)
        .expect("launch elementsd");
    let base = Node::from_elementsd(&daemon).expect("connect");
    base.rpc("createwallet", &["wallet".into()]).expect("createwallet");
    let node = base.for_wallet("wallet").expect("wallet client");
    node.rpc("rescanblockchain", &[]).expect("rescan");
    (daemon, node)
}

fn run(daemon: ElementsD, node: Node, reserve_seed: u64) -> Deployment {
    let oracle_keys = [keypair(7), keypair(8), keypair(9), keypair(101), keypair(102)];
    let (ctx, protocol) = ceremony(
        &node,
        oracle_keys.map(|k| k.x_only_public_key().0),
        reserve_seed,
        &crate::client::Confirm::SelfMine,
    )
    .expect("ceremony");
    Deployment { daemon, node, ctx, protocol, oracle_keys }
}

/// Launch the node and run the deployment ceremony against one large genesis coin.
pub fn deploy(reserve_seed: u64) -> Deployment {
    let (daemon, node) = launch(210_000_000_000);
    run(daemon, node, reserve_seed)
}

/// Deploy against a wallet fragmented into coins too small to fund the reserve individually,
/// the liquidtestnet faucet-drip case: the ceremony must select across several coins.
pub fn deploy_fragmented(reserve_seed: u64) -> Deployment {
    let (daemon, node) = launch(10_000_000);
    fragment(&node, 9);
    run(daemon, node, reserve_seed)
}

/// Split the wallet's single genesis coin into `n` equal coins with no large change left, so
/// no one coin can cover a reserve bigger than a single piece.
fn fragment(node: &Node, n: u64) {
    use elementsd::bitcoincore_rpc::jsonrpc::serde_json::json;
    let (op, sats, _) = node.biggest_coin().expect("coin");
    let each = (sats - crate::client::FEE_RPC) / n;
    let addrs: Vec<_> = (0..n).map(|_| node.new_address().expect("addr")).collect();
    let mut outs: Vec<(String, u64)> = addrs.iter().map(|a| (a.to_string(), each)).collect();
    outs.push(("fee".into(), sats - each * n)); // the remainder pays the fee
    let raw = node
        .rpc(
            "createrawtransaction",
            &[
                json!([{ "txid": op.txid.to_string(), "vout": op.vout }]),
                crate::client::out_array(&outs),
            ],
        )
        .expect("createrawtransaction");
    let signed = node.rpc("signrawtransactionwithwallet", &[raw]).expect("sign");
    let hex = signed["hex"].as_str().expect("signed hex").to_string();
    node.rpc("sendrawtransaction", &[hex.into()]).expect("broadcast");
    node.mine().expect("confirm fragmentation");
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
        let quotes = [0u8, 1, 2]
            .map(|i| (OracleSlot::new(i).unwrap(), sign_quote(&self.oracle_keys[i as usize], &payload)));
        OracleTick::new(payload.height, payload.backing_k, quotes).unwrap()
    }
}
