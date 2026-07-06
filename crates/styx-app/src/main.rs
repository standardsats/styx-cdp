//! styx-app: serve one wallet session on loopback. The token is printed once at startup;
//! the UI milestone will embed it into the served page instead.
//!
//! The tick loop is the binary's half of the lib's tick seam: assemble a quorum from the
//! relays (the same fetch walk-back the wallet CLI uses) and keep `AppState`'s tick fresh
//! for the op endpoints and the keeper loop alike.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use styx_app::api::{router, AppState};
use styx_app::auth::Gate;
use styx_app::config::AppConfig;
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::units::BlockHeight;
use styx_node::client::Node;
use styx_wallet::wallet::Wallet;
use styx_watch::config::StyxnetConfig;
use styx_watch::nostr::NostrQuotes;
use styx_watch::quotes::QuoteBook;

#[derive(Parser)]
#[command(about = "STYX v1 local application server")]
struct Args {
    /// Path to the app's TOML config (wallet purse + listener + keeper knobs).
    #[arg(long)]
    config: PathBuf,
}

/// How far below the tip the tick search walks; the wallet CLI's constant, same rationale
/// (well under the book's 32-block horizon - stalled oracles should fail loudly).
const TICK_WALKBACK: u32 = 8;

/// Keep the injected tick fresh from the relays. Absent relay or oracle transport keys is
/// not an error: the config is CLI-shaped, ticks stay test-injected, tick-needing ops
/// answer 409.
async fn tick_loop(
    net: StyxnetConfig,
    node: Node,
    oracle_pks: [XOnlyPublicKey; 5],
    state: Arc<AppState>,
    poll: Duration,
) {
    let authors: Vec<nostr_sdk::PublicKey> = net
        .oracles
        .iter()
        .filter_map(|o| o.nostr_pk.as_ref())
        .filter_map(|s| nostr_sdk::PublicKey::parse(s).ok())
        .collect();
    if authors.len() < 3 || net.nostr.relays.is_empty() {
        println!(
            "tick loop off: {} oracle transport keys, {} relays",
            authors.len(),
            net.nostr.relays.len()
        );
        return;
    }
    let sub = loop {
        match NostrQuotes::subscriber(&net.nostr.relays, authors.clone()).await {
            Ok(sub) => break sub,
            Err(e) => {
                eprintln!("relay connect: {e}, retrying");
                tokio::time::sleep(poll).await;
            }
        }
    };
    let node = Arc::new(node);
    loop {
        let n = node.clone();
        let tip = tokio::task::spawn_blocking(move || n.height()).await;
        if let Ok(Ok(tip)) = tip {
            let mut book = QuoteBook::new(oracle_pks);
            'walk: for h in (tip.saturating_sub(TICK_WALKBACK)..=tip).rev() {
                match sub.fetch(h, Duration::from_secs(5)).await {
                    Ok(quotes) => {
                        for q in &quotes {
                            let _ = book.insert(q, BlockHeight::new(tip));
                        }
                        if let Some(tick) = book.assemble_tick(BlockHeight::new(h)) {
                            state.set_tick(tick);
                            state.emit("tick", format!("height {h}"));
                            break 'walk;
                        }
                    }
                    Err(e) => eprintln!("quote fetch at {h}: {e}"),
                }
            }
        }
        tokio::time::sleep(poll).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let cfg = AppConfig::load(&args.config)?;
    let addr = cfg.listen_addr()?;
    let wallet = Wallet::open_session(&cfg.purse)?;
    let oracle_pks = wallet.ctx.params.oracle_pks;

    let gate = Arc::new(Gate::mint(addr));
    let poll = Duration::from_millis(cfg.poll_ms);
    let state = AppState::new(wallet, cfg.keeper.opts(), poll);

    let net = StyxnetConfig::load(&cfg.purse.styxnet)?;
    let node = Node::from_url(&cfg.purse.rpc_url, cfg.purse.auth()?)?;
    tokio::spawn(tick_loop(net, node, oracle_pks, state.clone(), poll));

    println!("styx-app up: http://{addr}/api/status");
    println!("  token: {}", gate.token());
    println!("  every request needs the X-Styx-Token header");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let serve = axum::serve(listener, router(state.clone(), gate));
    tokio::select! {
        r = serve => r.map_err(Into::into),
        _ = tokio::signal::ctrl_c() => {
            // A running keeper loop lives on the blocking pool, and dropping the runtime
            // WAITS for blocking tasks: without this the process hangs unkillable by a
            // second Ctrl-C (the signal handler owns SIGINT). One poll interval bounds
            // the wait.
            state.request_keeper_stop();
            println!("shutting down");
            Ok(())
        }
    }
}
