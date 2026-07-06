//! styx-keeper: the watchtower daemon. Subscribes to the oracle quotes, keeps the indexer
//! synced, and once per fresh tick performs the single most urgent duty: liquidate what
//! must be liquidated, poke the mint anchor toward the tip, refresh dormant healthy vaults.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let cfg = KeeperConfig::load(&args.config)?;
    let net = StyxnetConfig::load(&cfg.purse.styxnet)?;
    let purse = Wallet::open_session(&cfg.purse)?;
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

    loop {
        // Sync BEFORE draining: the book windows incoming quotes against the indexed tip,
        // and a stale tip would refuse fresh quotes as "future" after a block burst.
        keeper.purse.sync()?;
        keeper.drain(&mut transport).await;
        if let Some(tick) = keeper.assemble() {
            match keeper.step(&tick) {
                Ok(Some(done)) => println!("performed: {done:?}"),
                Ok(None) => {}
                // A Rejected alert means a builder invariant broke: stop rather than spin.
                Err(e @ KeeperError::Rejected { .. }) => return Err(e.into()),
                Err(e) => eprintln!("step failed ({e}), continuing"),
            }
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
