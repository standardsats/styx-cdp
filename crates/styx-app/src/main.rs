//! styx-app: serve one wallet session on loopback. The token is printed once at startup;
//! the UI milestone will embed it into the served page instead.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use styx_app::api::{router, AppState};
use styx_app::auth::Gate;
use styx_app::config::AppConfig;
use styx_wallet::wallet::Wallet;

#[derive(Parser)]
#[command(about = "STYX v1 local application server")]
struct Args {
    /// Path to the app's TOML config (wallet purse + listener).
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let cfg = AppConfig::load(&args.config)?;
    let addr = cfg.listen_addr()?;
    let wallet = Wallet::open_session(&cfg.purse)?;

    let gate = Arc::new(Gate::mint(addr));
    let state = AppState::new(wallet);
    println!("styx-app up: http://{addr}/api/status");
    println!("  token: {}", gate.token());
    println!("  every request needs the X-Styx-Token header");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let serve = axum::serve(listener, router(state, gate));
    tokio::select! {
        r = serve => r.map_err(Into::into),
        _ = tokio::signal::ctrl_c() => {
            println!("shutting down");
            Ok(())
        }
    }
}
