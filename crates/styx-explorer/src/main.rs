//! styx-explorer: sync the indexer from its own node, watch the public relay for quote
//! recency and the current tick, serve the read-only page.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use styx_core::artifacts::{Artifacts, Ctx};
use styx_core::units::BlockHeight;
use styx_explorer::config::ExplorerConfig;
use styx_explorer::serve::{router, App};
use styx_explorer::state::ExplorerState;
use styx_node::client::Node;
use styx_watch::config::StyxnetConfig;
use styx_watch::index::IndexState;
use styx_watch::nostr::NostrQuotes;
use styx_watch::quotes::QuoteBook;
use styx_watch::snapshot;
use styx_watch::sync::{catch_up, SyncError};
use styx_watch::transport::QuoteTransport;

#[derive(Parser)]
#[command(about = "STYX v1 public explorer")]
struct Args {
    #[arg(long)]
    config: PathBuf,
}

/// Sync forever into a WORKING COPY and swap under a short write lock: the RPC round-trips
/// (and the from-anchor rescan after a reorg, and the long first sync) must never hold the
/// lock the page renders under - the hosted surface keeps serving a lagging-but-valid view.
fn sync_loop(
    node: Node,
    ctx: Ctx,
    anchor: u32,
    snapshot_path: PathBuf,
    state: Arc<ExplorerState>,
    poll: Duration,
) {
    loop {
        let mut work = state.index.read().unwrap_or_else(|e| e.into_inner()).clone();
        match catch_up(&node, &ctx, &[], &mut work) {
            Ok(notices) => {
                if !notices.is_empty() {
                    if let Err(e) = snapshot::save(&work, &snapshot_path) {
                        eprintln!("snapshot: {e}");
                    }
                }
                *state.index.write().unwrap_or_else(|e| e.into_inner()) = work;
                state.push_events(notices);
            }
            Err(SyncError::Reorg { height }) => {
                // Correctness over continuity: the explorer holds no funds, rescan.
                eprintln!("reorg at {height}: rescanning from the anchor");
                match node.block_hash(anchor) {
                    Ok(hash) => {
                        *state.index.write().unwrap_or_else(|e| e.into_inner()) =
                            IndexState::at_height(anchor, hash);
                    }
                    Err(e) => eprintln!("anchor hash: {e}"),
                }
            }
            Err(e) => eprintln!("sync: {e}"),
        }
        std::thread::sleep(poll);
    }
}

/// Watch the relay: every verified quote stamps its slot's recency; the book assembles the
/// current tick at the indexed tip. A dead notification stream tears the subscriber down
/// and reconnects - recency must not silently freeze on a relay restart.
async fn quote_loop(
    net: StyxnetConfig,
    oracle_pks: [styx_core::elements::secp256k1_zkp::XOnlyPublicKey; 5],
    node: Node,
    state: Arc<ExplorerState>,
) {
    let authors: Vec<nostr_sdk::PublicKey> = net
        .oracles
        .iter()
        .filter_map(|o| o.nostr_pk.as_ref())
        .filter_map(|s| nostr_sdk::PublicKey::parse(s).ok())
        .collect();
    if authors.is_empty() || net.nostr.relays.is_empty() {
        println!("quote loop off: no relay or oracle transport keys configured");
        return;
    }
    let mut book = QuoteBook::new(oracle_pks);
    'reconnect: loop {
        let mut sub = match NostrQuotes::subscriber(&net.nostr.relays, authors.clone()).await {
            Ok(sub) => sub,
            Err(e) => {
                eprintln!("relay connect: {e}, retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue 'reconnect;
            }
        };
        loop {
            match sub.recv().await {
                Ok(q) => {
                    let tip = state.index.read().unwrap_or_else(|e| e.into_inner()).height;
                    if book.insert(&q, BlockHeight::new(tip)).is_ok() {
                        state.saw_slot(q.slot as usize, &q.name, q.price);
                        for h in (tip.saturating_sub(8)..=tip).rev() {
                            if let Some(tick) = book.assemble_tick(BlockHeight::new(h)) {
                                // The tick height's block hash, for the explorer's block link.
                                let hash = node.block_hash(tick.height().raw()).ok();
                                state.set_tick(tick, hash);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("relay stream: {e}, resubscribing");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue 'reconnect;
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let cfg = ExplorerConfig::load(&args.config)?;
    let addr = cfg.listen_addr()?;
    let net = StyxnetConfig::load(&cfg.styxnet)?;
    let params = net.params()?;
    let genesis = net.genesis()?;
    let anchor = net
        .protocol
        .as_ref()
        .map(|p| p.issuer_anchor_genesis)
        .ok_or("styxnet.toml has no [protocol] section (run the ceremony first)")?;

    let node = Node::from_url(&cfg.rpc_url, cfg.auth()?)?;
    // A second RPC handle for the quote loop (the first is moved into the sync loop): it
    // resolves each tick height to a block hash for the explorer's block link.
    let quote_node = Node::from_url(&cfg.rpc_url, cfg.auth()?)?;
    let node_genesis = node.genesis()?;
    if node_genesis != genesis {
        return Err(format!("genesis mismatch: config {genesis}, node {node_genesis}").into());
    }
    let artifacts = Artifacts::compile(&params).map_err(|e| e.to_string())?;
    let oracle_pks = params.oracle_pks;
    let ctx = Ctx { params, artifacts, genesis };

    let index = if cfg.snapshot.exists() {
        snapshot::load(&cfg.snapshot)?
    } else {
        IndexState::at_height(anchor, node.block_hash(anchor)?)
    };
    let state = ExplorerState::new(index, net.nostr.relays.first().cloned());

    let poll = Duration::from_millis(cfg.poll_ms);
    {
        let (state, snapshot_path) = (state.clone(), cfg.snapshot.clone());
        tokio::task::spawn_blocking(move || sync_loop(node, ctx, anchor, snapshot_path, state, poll));
    }
    tokio::spawn(quote_loop(net, oracle_pks, quote_node, state.clone()));

    let app = Arc::new(App { state, app_url: cfg.app_url.clone() });
    println!("styx-explorer up: http://{addr}/");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tokio::select! {
        r = axum::serve(listener, router(app)) => r.map_err(Into::into),
        _ = tokio::signal::ctrl_c() => {
            println!("shutting down");
            std::process::exit(0); // the sync loop is a blocking task; do not wait for it
        }
    }
}
