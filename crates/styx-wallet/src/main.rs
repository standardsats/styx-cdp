//! styx-wallet: the owner's CLI. Commands sync the indexer, assemble a tick from the
//! relays when the op needs one, and drive the library ops; everything protocol-critical
//! lives in the lib and is exercised by the on-node tests.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use clap::{Parser, Subcommand};
use styx_core::elements::{OutPoint, Txid};
use styx_core::oracle::OracleTick;
use styx_core::units::{BlockHeight, Obol, Sats};
use styx_node::client::Node;
use styx_wallet::config::WalletConfig;
use styx_wallet::wallet::Wallet;
use styx_watch::config::StyxnetConfig;
use styx_watch::nostr::NostrQuotes;
use styx_watch::quotes::QuoteBook;

#[derive(Parser)]
#[command(about = "STYX v1 owner wallet")]
struct Cli {
    /// Path to the wallet's TOML config (not needed for keygen).
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a fresh owner + funding key pair (paste into the wallet config).
    Keygen,
    /// Show the funding address and the owner pubkey.
    Address,
    /// Move sats from the node's own wallet to the funding address.
    Fund {
        #[arg(long)]
        sats: u64,
    },
    /// Protocol state, our vaults, and the wallet's coins.
    Status,
    /// Open a vault: lock collateral, mint `principal` OBOL.
    Open {
        /// Principal in OBOL units (= debt cents).
        #[arg(long)]
        principal: u64,
        /// Collateral in sats; defaults to the amount for --cr (or the 150% minimum).
        #[arg(long)]
        collateral: Option<u64>,
        /// Target collateral ratio in percent, used when --collateral is omitted.
        #[arg(long)]
        cr: Option<u32>,
    },
    /// Repay part of the debt.
    Repay {
        #[arg(long)]
        vault: Option<String>,
        #[arg(long)]
        amount: u64,
    },
    /// Draw more OBOL against the vault.
    Draw {
        #[arg(long)]
        vault: Option<String>,
        #[arg(long)]
        amount: u64,
    },
    /// Advance the vault's freshness ratchet.
    Refresh {
        #[arg(long)]
        vault: Option<String>,
    },
    /// Repay the whole debt and free the collateral.
    Close {
        #[arg(long)]
        vault: Option<String>,
    },
    /// Redeem `x` OBOL for collateral against one of our vaults.
    Redeem {
        #[arg(long)]
        vault: Option<String>,
        #[arg(long)]
        x: u64,
    },
}

fn parse_outpoint(s: &str) -> Result<OutPoint, String> {
    let (txid, vout) = s.split_once(':').ok_or("expected txid:vout")?;
    Ok(OutPoint::new(
        Txid::from_str(txid).map_err(|e| e.to_string())?,
        vout.parse().map_err(|_| "bad vout".to_string())?,
    ))
}

fn vault_arg(v: &Option<String>) -> Result<Option<OutPoint>, String> {
    v.as_ref().map(|s| parse_outpoint(s)).transpose()
}

/// How far below the tip the tick search walks. Deliberately well under the quote book's
/// 32-block horizon: a quorum older than a few blocks means the oracles are stalled, and
/// failing loudly beats quietly signing intents against stale prices.
const TICK_WALKBACK: u32 = 8;

/// Assemble a tick from the relays: fetch stored quotes for the tip (walking back a few
/// blocks if the oracles have not caught up) and take the first height with a quorum.
async fn assemble_tick(
    net: &StyxnetConfig,
    node: &Node,
    wallet: &Wallet,
) -> Result<OracleTick, Box<dyn std::error::Error>> {
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
    let sub = NostrQuotes::subscriber(&net.nostr.relays, authors).await?;
    let tip = node.height()?;
    let mut book = QuoteBook::new(wallet.ctx.params.oracle_pks);
    for h in (tip.saturating_sub(TICK_WALKBACK)..=tip).rev() {
        for q in sub.fetch(h, Duration::from_secs(5)).await? {
            if let Err(e) = book.insert(&q, BlockHeight::new(tip)) {
                eprintln!("dropping a quote: {e}");
            }
        }
        if let Some(tick) = book.assemble_tick(BlockHeight::new(h)) {
            return Ok(tick);
        }
    }
    Err(format!("no oracle quorum within {TICK_WALKBACK} blocks of the tip").into())
}

fn print_report(label: &str, report: &styx_wallet::ops::OpReport) {
    println!("{label}: broadcast {}", report.txid);
    if let Some(v) = &report.vault {
        println!(
            "vault {}: debt {} coll {} last_height {}",
            v.outpoint,
            v.state.debt.raw(),
            v.value.raw(),
            v.state.last_height.raw()
        );
    }
    println!("(wait for a block, then `status` reflects it)");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    if let Cmd::Keygen = cli.cmd {
        // OS entropy via the nostr key generator; both curves are secp256k1.
        let owner = nostr_sdk::Keys::generate();
        let funding = nostr_sdk::Keys::generate();
        println!("owner_seckey = \"{}\"", owner.secret_key().to_secret_hex());
        println!("funding_seckey = \"{}\"", funding.secret_key().to_secret_hex());
        return Ok(());
    }

    let config_path = cli.config.ok_or("--config is required")?;
    let cfg = WalletConfig::load(&config_path)?;
    let net = StyxnetConfig::load(&cfg.styxnet)?;
    let mut wallet = Wallet::open_session(&cfg)?;
    wallet.sync()?;

    match cli.cmd {
        Cmd::Keygen => unreachable!("handled above"),
        Cmd::Address => {
            println!("funding address: {}", wallet.funding_address());
            println!("owner pubkey:    {}", wallet.owner_pk());
        }
        Cmd::Fund { sats } => {
            let op = wallet.fund(sats)?;
            println!("funded {sats} sats -> {op}");
        }
        Cmd::Status => {
            println!("height {}", wallet.state.height);
            match wallet.state.protocol() {
                Some(p) => println!(
                    "pot {} OBOL | reserve {} sats | issuer anchor {}",
                    p.pot.value.raw(),
                    p.reserve.value.raw(),
                    p.issuer.state.last_mint_height.raw()
                ),
                None => println!("protocol not live yet"),
            }
            let vaults = wallet.my_vaults();
            println!("vaults: {}", vaults.len());
            for v in &vaults {
                println!(
                    "  {} debt {} coll {} last_height {}",
                    v.outpoint,
                    v.state.debt.raw(),
                    v.value.raw(),
                    v.state.last_height.raw()
                );
            }
            let lbtc = wallet.lbtc_coins()?;
            let obol = wallet.obol_coins()?;
            println!(
                "L-BTC: {} sats in {} coins | OBOL: {} units in {} coins",
                lbtc.iter().map(|(_, v)| v).sum::<u64>(),
                lbtc.len(),
                obol.iter().map(|(_, v)| v).sum::<u64>(),
                obol.len()
            );
        }
        Cmd::Open { principal, collateral, cr } => {
            let tick = assemble_tick(&net, &wallet.node, &wallet).await?;
            let principal = Obol::new(principal);
            let collateral = match collateral {
                Some(c) => Sats::new(c),
                None => Wallet::collateral_for(principal, &tick, cr)?,
            };
            let report = wallet.open(principal, collateral, &tick)?;
            print_report("open", &report);
        }
        Cmd::Repay { vault, amount } => {
            let report = wallet.repay(vault_arg(&vault)?, Obol::new(amount))?;
            print_report("repay", &report);
        }
        Cmd::Draw { vault, amount } => {
            let tick = assemble_tick(&net, &wallet.node, &wallet).await?;
            let report = wallet.draw(vault_arg(&vault)?, Obol::new(amount), &tick)?;
            print_report("draw", &report);
        }
        Cmd::Refresh { vault } => {
            let tick = assemble_tick(&net, &wallet.node, &wallet).await?;
            let report = wallet.refresh(vault_arg(&vault)?, &tick)?;
            print_report("refresh", &report);
        }
        Cmd::Close { vault } => {
            let report = wallet.close(vault_arg(&vault)?)?;
            print_report("close", &report);
        }
        Cmd::Redeem { vault, x } => {
            let tick = assemble_tick(&net, &wallet.node, &wallet).await?;
            let report = wallet.redeem(vault_arg(&vault)?, Obol::new(x), &tick)?;
            print_report("redeem", &report);
        }
    }
    Ok(())
}
