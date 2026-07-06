//! The oracle's running state and its per-block duty: sign the current price at every new
//! tip and publish it. The price lives behind a lock so the admin endpoint can move it
//! between blocks (scenario control); a real feed later slots in behind the same setter.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use styx_core::consts::K_PAR;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::oracle::{OracleSlot, TickPayload};
use styx_core::units::{BlockHeight, Price};
use styx_node::NodeError;
use styx_watch::quotes::WireQuote;
use styx_watch::transport::QuoteTransport;

pub struct OracleState {
    slot: OracleSlot,
    keypair: zkp::Keypair,
    price: RwLock<Price>,
    /// The last height a quote was published for; 0 means none yet.
    pub last_published: AtomicU32,
}

impl OracleState {
    pub fn new(slot: OracleSlot, keypair: zkp::Keypair, initial: Price) -> Self {
        OracleState { slot, keypair, price: RwLock::new(initial), last_published: AtomicU32::new(0) }
    }

    pub fn slot(&self) -> OracleSlot {
        self.slot
    }

    pub fn price(&self) -> Price {
        *self.price.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_price(&self, price: Price) {
        *self.price.write().unwrap_or_else(|e| e.into_inner()) = price;
    }

    /// Sign the current price for a height. v1 publishes par backing (the reserve backstop
    /// phase computes a real backing_k later).
    pub fn quote_for(&self, height: BlockHeight) -> WireQuote {
        let payload = TickPayload { height, price: self.price(), backing_k: K_PAR };
        WireQuote::sign(&self.keypair, self.slot, &payload)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("node unreachable for {secs}s: {last}")]
    NodeDown { secs: u64, last: NodeError },
    #[error("join: {0}")]
    Join(String),
}

/// How long the node may stay unreachable before the daemon gives up (`main`'s choice; the
/// loop takes it as a parameter so tests can shrink it).
pub const NODE_DOWN_FATAL: Duration = Duration::from_secs(60);

/// The block loop: poll the tip, publish one quote per new height. `height` is the node
/// call, run on the blocking pool (the RPC client is synchronous).
///
/// Failure policy: the anchor's freshness is system liveness (the POKE loop feeds on these
/// quotes), so the loop dies only for what cannot heal. A transport error is a relay
/// hiccup: logged and retried on the next poll - `last_published` advances only after a
/// successful publish, so the same height is re-signed and re-sent. A node error is
/// tolerated until it has lasted `node_down_fatal`.
pub async fn publish_blocks<T: QuoteTransport>(
    state: Arc<OracleState>,
    transport: &mut T,
    height: impl Fn() -> Result<u32, NodeError> + Clone + Send + 'static,
    poll: Duration,
    node_down_fatal: Duration,
) -> Result<(), RunError> {
    let mut node_down_since: Option<std::time::Instant> = None;
    loop {
        let call = height.clone();
        match tokio::task::spawn_blocking(call).await.map_err(|e| RunError::Join(e.to_string()))? {
            Err(e) => {
                let since = *node_down_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= node_down_fatal {
                    return Err(RunError::NodeDown { secs: since.elapsed().as_secs(), last: e });
                }
                eprintln!("node poll failed ({e}), retrying");
            }
            Ok(h) => {
                node_down_since = None;
                if h > state.last_published.load(Ordering::SeqCst) {
                    let quote = state.quote_for(BlockHeight::new(h));
                    match transport.publish(&quote).await {
                        Ok(()) => {
                            state.last_published.store(h, Ordering::SeqCst);
                            println!(
                                "quote published: slot {} height {} price {}",
                                quote.slot, quote.height, quote.price
                            );
                        }
                        Err(e) => eprintln!("publish failed at height {h} ({e}), retrying"),
                    }
                }
            }
        }
        tokio::time::sleep(poll).await;
    }
}
