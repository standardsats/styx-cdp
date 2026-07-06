//! styx-deploy: the one-time deployment against a live styxnet node, and the standing
//! verification that a config's compiled artifacts match what is actually on chain.
//!
//! `run` reads the operator's skeleton (oracles + relays), executes the ceremony (two
//! non-reissuable issuances, reserve seed) on the node's funded wallet, and completes the
//! config with the chain-derived sections. `verify` recompiles the artifacts from the
//! config (preflight inside) and locates the protocol singletons at the derived addresses -
//! the compiled-pins-versus-published-artifacts check.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use styx_node::client::Auth;
use styx_node::client::Node;
use styx_node::scan::{find_issuer, scan_protocol};
use styx_pset::Ctx;
use styx_watch::config::{Assets, Protocol, StyxnetConfig};

#[derive(Parser)]
#[command(name = "styx-deploy", about)]
struct Cli {
    /// The node's RPC endpoint, e.g. http://127.0.0.1:18884
    #[arg(long)]
    rpc_url: String,
    #[arg(long)]
    rpc_user: Option<String>,
    #[arg(long)]
    rpc_password: Option<String>,
    /// Cookie-file auth (harness nodes); overrides user/password.
    #[arg(long)]
    rpc_cookie: Option<PathBuf>,
    /// The styxnet.toml to read (and, for `run`, complete in place).
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute the deployment ceremony and complete the config.
    Run {
        /// The initial stability reserve, in sats.
        #[arg(long, default_value_t = 18_000_000)]
        reserve_seed: u64,
        /// The wallet to create/load on the node before the ceremony.
        #[arg(long, default_value = "styx-deploy")]
        wallet: String,
        /// Fill an empty oracle set with the five well-known test keys. Development and
        /// swarm rehearsals only: the secrets are public.
        #[arg(long)]
        test_oracles: bool,
        /// Mine the ceremony's blocks on this node (a lone regtest/private node). Without
        /// it the ceremony waits for whoever produces blocks on this chain - a producer
        /// daemon or the testnet federation.
        #[arg(long)]
        self_mine: bool,
        /// How long to wait for each confirmation before giving up (a stalled federation
        /// should not kill a deployment silently at some baked-in limit).
        #[arg(long, default_value_t = 300)]
        confirm_timeout: u64,
    },
    /// Verify a completed config against the chain.
    Verify {
        /// Upper bound for the issuer-anchor recovery scan (default: the chain tip).
        #[arg(long)]
        scan_to: Option<u32>,
    },
}

fn auth(cli: &Cli) -> Auth {
    if let Some(cookie) = &cli.rpc_cookie {
        Auth::CookieFile(cookie.clone())
    } else {
        Auth::UserPass(
            cli.rpc_user.clone().unwrap_or_default(),
            cli.rpc_password.clone().unwrap_or_default(),
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let node = Node::from_url(&cli.rpc_url, auth(&cli))?;
    let mut cfg = StyxnetConfig::load(&cli.config)?;

    match &cli.command {
        Command::Run { reserve_seed, wallet, test_oracles, self_mine, confirm_timeout } => {
            if cfg.assets.is_some() {
                return Err("this config is already deployed (assets present); \
                            start from a fresh skeleton to redeploy"
                    .into());
            }
            if *test_oracles && !cfg.oracles.is_empty() {
                return Err("--test-oracles requires an empty oracle set; \
                            this config already lists oracles"
                    .into());
            }
            if *test_oracles {
                for (slot, secret) in [7u8, 8, 9, 101, 102].into_iter().enumerate() {
                    let mut sk = [0u8; 32];
                    sk[31] = secret;
                    let kp = styx_core::elements::secp256k1_zkp::Keypair::from_seckey_slice(
                        styx_core::secp(),
                        &sk,
                    )?;
                    cfg.oracles.push(styx_watch::config::OracleEntry {
                        slot: slot as u8,
                        protocol_pk: kp.x_only_public_key().0.to_string(),
                        nostr_pk: None,
                        admin_url: None,
                    });
                }
            }
            let oracle_pks = cfg.oracle_pks()?;
            // Idempotent wallet preparation: create-or-load either way, then confirm the
            // wallet is actually usable. Swallowing create/load lets a re-run reuse an
            // existing wallet; the getwalletinfo probe turns a real failure (permissions, a
            // corrupt wallet) into a clear error here instead of a cryptic "listunspent
            // empty" during the ceremony.
            let _ = node.rpc("createwallet", &[wallet.as_str().into()]);
            let _ = node.rpc("loadwallet", &[wallet.as_str().into()]);
            let node = node.for_wallet(wallet)?;
            node.rpc("getwalletinfo", &[]).map_err(|e| format!("wallet {wallet} is not usable: {e}"))?;
            // A fresh wallet must scan from genesis to see the initialfreecoins output.
            let _ = node.rpc("rescanblockchain", &[]);
            let anchor = node.height()?;
            let confirm = if *self_mine {
                styx_node::client::Confirm::SelfMine
            } else {
                styx_node::client::Confirm::Await {
                    timeout: std::time::Duration::from_secs(*confirm_timeout),
                }
            };
            let (ctx, protocol) =
                styx_node::ceremony::ceremony(&node, oracle_pks, *reserve_seed, &confirm)?;
            cfg.network.genesis = Some(ctx.genesis.to_string());
            cfg.assets = Some(Assets {
                obol: ctx.params.obol.to_string(),
                issuer_token: ctx.params.issuer_token.to_string(),
                policy: ctx.params.policy.to_string(),
            });
            cfg.protocol = Some(Protocol { issuer_anchor_genesis: anchor });
            cfg.save(&cli.config)?;
            println!("deployed:");
            println!("  genesis       {}", ctx.genesis);
            println!("  obol          {}", ctx.params.obol);
            println!("  issuer token  {}", ctx.params.issuer_token);
            println!(
                "  pot           {} ({} OBOL units)",
                protocol.pot.outpoint,
                protocol.pot.value.raw()
            );
            println!(
                "  reserve       {} ({} sats)",
                protocol.reserve.outpoint,
                protocol.reserve.value.raw()
            );
            println!(
                "  issuer        {} (anchor {}, scan floor {})",
                protocol.issuer.outpoint,
                protocol.issuer.state.last_mint_height.raw(),
                anchor
            );
            println!("config completed: {}", cli.config.display());
            Ok(())
        }
        Command::Verify { scan_to } => {
            let params = cfg.params()?;
            let genesis = cfg.genesis()?;
            let node_genesis = node.genesis()?;
            if node_genesis != genesis {
                return Err(format!(
                    "genesis mismatch: config {genesis}, node {node_genesis} - wrong network"
                )
                .into());
            }
            // Recompiling from the config's params re-derives every cross-covenant pin;
            // preflight runs inside compile.
            let artifacts = styx_core::artifacts::Artifacts::compile(&params)?;
            let ctx = Ctx { params, artifacts, genesis };
            let floor = cfg
                .protocol
                .as_ref()
                .map(|p| p.issuer_anchor_genesis)
                .ok_or("config has no [protocol] section")?;
            let tip = scan_to.unwrap_or(node.height()?);
            let issuer = find_issuer(&node, &ctx, floor..=tip)?;
            let protocol = scan_protocol(&node, &ctx, issuer.state)?;
            println!("verified against the chain:");
            println!("  genesis  {genesis}");
            println!("  pot      {} ({} OBOL units)", protocol.pot.outpoint, protocol.pot.value.raw());
            println!("  reserve  {} ({} sats)", protocol.reserve.outpoint, protocol.reserve.value.raw());
            println!(
                "  issuer   {} (anchor {})",
                protocol.issuer.outpoint,
                issuer.state.last_mint_height.raw()
            );
            Ok(())
        }
    }
}
