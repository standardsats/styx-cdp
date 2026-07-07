//! styx-oracle: one quorum slot as a daemon. Signs the configured price at every new block
//! of its own elementsd, publishes the quote to the Nostr relays, and serves the
//! debug/admin HTTP surface.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::oracle::OracleSlot;
use styx_core::units::Price;
use styx_node::client::Node;
use styx_watch::nostr::NostrQuotes;

use styx_oracle::config::OracleConfig;
use styx_oracle::http;
use styx_oracle::service::{publish_blocks, OracleState, NODE_DOWN_FATAL};

#[derive(Parser)]
#[command(about = "STYX v1 oracle daemon")]
struct Args {
    /// Path to the oracle's TOML config (required to run; not needed for keygen).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Generate a fresh protocol + nostr keypair for a new oracle slot and exit.
    #[arg(long)]
    keygen: bool,
}

fn parse_protocol_key(hex: &str) -> Result<zkp::Keypair, String> {
    let mut sk = [0u8; 32];
    if hex.len() != 64 {
        return Err("protocol_seckey must be 32 bytes of hex".into());
    }
    for (i, byte) in sk.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "protocol_seckey: bad hex".to_string())?;
    }
    zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).map_err(|e| e.to_string())
}

/// Print an oracle identity: the secrets in oracle-config form, the pubkeys in
/// styxnet.toml form. OS entropy via the nostr key generator; both curves are secp256k1.
fn keygen() -> Result<(), Box<dyn std::error::Error>> {
    let protocol = nostr_sdk::Keys::generate();
    let nostr = nostr_sdk::Keys::generate();
    let kp = parse_protocol_key(&protocol.secret_key().to_secret_hex())?;
    println!("# oracle config (secrets):");
    println!("protocol_seckey = \"{}\"", protocol.secret_key().to_secret_hex());
    println!("nostr_seckey = \"{}\"", nostr.secret_key().to_secret_hex());
    println!("# styxnet.toml entry (public):");
    println!("protocol_pk = \"{}\"", kp.x_only_public_key().0);
    println!("nostr_pk = \"{}\"", nostr.public_key().to_hex());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.keygen {
        return keygen();
    }
    let config = args.config.ok_or("--config is required (or --keygen)")?;
    let cfg = OracleConfig::load(&config)?;
    let slot = OracleSlot::new(cfg.slot).map_err(|e| e.to_string())?;
    let keypair = parse_protocol_key(&cfg.protocol_seckey)?;
    let nostr_keys = nostr_sdk::Keys::parse(&cfg.nostr_seckey)?;

    let max_feed_age =
        cfg.feed.as_ref().and_then(|f| f.max_age_secs).map(std::time::Duration::from_secs);
    let state = Arc::new(
        OracleState::new(slot, keypair, Price::new(cfg.price_usd))
            .with_max_feed_age(max_feed_age)
            .with_name(cfg.name.clone()),
    );
    let node = Node::from_url(&cfg.rpc_url, cfg.auth()?)?;
    let mut transport = NostrQuotes::publisher(&cfg.relays, nostr_keys).await?;

    // The live price backend, when configured: updates land under any sticky override.
    if let Some(feed_cfg) = &cfg.feed {
        let backend: styx_oracle::feed::Backend = feed_cfg.backend.parse()?;
        let source = styx_oracle::feed::HttpFeed::new(backend, feed_cfg.url.clone())?;
        let feed_state = state.clone();
        let poll = Duration::from_millis(feed_cfg.poll_ms);
        tokio::spawn(styx_oracle::service::run_feed(source, feed_state, poll));
        println!("feed: {} every {}ms", backend.name(), feed_cfg.poll_ms);
    }

    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    println!(
        "styx-oracle slot {} up: admin http://{} price {} poll {}ms",
        cfg.slot, cfg.listen, cfg.price_usd, cfg.poll_ms
    );

    let admin = axum::serve(listener, http::router(state.clone()));
    let node = Arc::new(node);
    let height = move || node.height();
    let blocks = publish_blocks(
        state,
        &mut transport,
        height,
        Duration::from_millis(cfg.poll_ms),
        NODE_DOWN_FATAL,
    );

    tokio::select! {
        r = admin => r.map_err(Into::into),
        r = blocks => r.map_err(Into::into),
        _ = tokio::signal::ctrl_c() => {
            println!("shutting down");
            Ok(())
        }
    }
}
