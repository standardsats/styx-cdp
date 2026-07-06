//! styx-keeper: the watchtower daemon. Subscribes to the oracle quotes, keeps the indexer
//! synced, and once per fresh tick performs the single most urgent duty: liquidate what
//! must be liquidated, poke the mint anchor toward the tip, refresh dormant healthy vaults.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use styx_keeper::config::KeeperConfig;
use styx_keeper::keeper::{Keeper, KeeperError};
use styx_wallet::wallet::Wallet;
use styx_watch::config::StyxnetConfig;
use styx_watch::nostr::NostrQuotes;

#[derive(Parser)]
#[command(about = "STYX v1 keeper daemon")]
struct Args {
    /// Path to the keeper's TOML config.
    #[arg(long)]
    config: PathBuf,
}

/// How long elementsd may stay unreachable before the daemon gives up. Generous compared
/// to the oracle's window on purpose: under compose the keeper does not auto-restart
/// (exit 65 is the invariant alert, and docker cannot filter exit codes), so dying here
/// is permanent - and a node restarting through RPC warmup takes minutes on a real
/// datadir.
const NODE_DOWN_FATAL: Duration = Duration::from_secs(600);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let cfg = KeeperConfig::load(&args.config)?;
    let net = StyxnetConfig::load(&cfg.purse.styxnet)?;
    // The session open touches the node (genesis cross-check); under compose the node
    // may still be warming up when the keeper starts. Tolerate exactly the node-down
    // errors, bounded; anything else (config, artifacts) fails immediately.
    let boot = Instant::now();
    let purse = loop {
        match Wallet::open_session(&cfg.purse) {
            Ok(w) => break w,
            Err(e) if e.is_node_down() && boot.elapsed() < NODE_DOWN_FATAL => {
                eprintln!("node not ready ({e}), retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e.into()),
        }
    };
    let mut keeper = Keeper::new(purse, cfg.opts());

    let authors: Vec<nostr_sdk::PublicKey> = net
        .oracles
        .iter()
        .filter_map(|o| o.nostr_pk.as_ref())
        .map(|s| nostr_sdk::PublicKey::parse(s))
        .collect::<Result<_, _>>()?;
    if authors.len() < 3 {
        return Err(
            format!("styxnet.toml lists {} oracle nostr keys; a quorum needs 3", authors.len()).into()
        );
    }
    let mut transport = NostrQuotes::subscriber(&net.nostr.relays, authors).await?;
    println!(
        "styx-keeper up: poke_lag {} refresh_lag {} poll {}ms",
        cfg.poke_lag, cfg.refresh_lag, cfg.poll_ms
    );

    let mut node_down_since: Option<Instant> = None;
    loop {
        // Sync BEFORE draining: the book windows incoming quotes against the indexed tip,
        // and a stale tip would refuse fresh quotes as "future" after a block burst.
        match keeper.purse.sync() {
            Ok(_) => {
                node_down_since = None;
                keeper.drain(&mut transport).await;
                if let Some(tick) = keeper.assemble() {
                    match keeper.step(&tick) {
                        Ok(Some(done)) => println!("performed: {done:?}"),
                        Ok(None) => {}
                        // A Rejected alert means a builder invariant broke: stop rather
                        // than spin, with the exit code the systemd unit refuses to
                        // restart.
                        Err(e @ KeeperError::Rejected { .. }) => {
                            eprintln!("{e}");
                            std::process::exit(styx_keeper::keeper::REJECTED_EXIT);
                        }
                        Err(e) => eprintln!("step failed ({e}), continuing"),
                    }
                }
            }
            // A node hiccup (elementsd restart, RPC warmup) must not kill a daemon that
            // will not be restarted: same bounded tolerance as the oracle's block loop.
            Err(e) if e.is_node_down() => {
                let since = *node_down_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= NODE_DOWN_FATAL {
                    return Err(format!(
                        "node unreachable for {}s: {e}",
                        since.elapsed().as_secs()
                    )
                    .into());
                }
                eprintln!("sync failed ({e}), node down {}s, retrying", since.elapsed().as_secs());
            }
            Err(e) => return Err(e.into()),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(cfg.poll_ms)) => {}
            _ = tokio::signal::ctrl_c() => {
                println!("shutting down");
                return Ok(());
            }
        }
    }
}
