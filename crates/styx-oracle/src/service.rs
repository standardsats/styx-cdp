//! The oracle's running state and its per-block duty: sign the current price at every new
//! tip and publish it.
//!
//! The effective price layers two sources: the FEED (a `PriceSource` backend updating in
//! the background, or the config's starting price when no backend is configured) under a
//! sticky manual OVERRIDE (POST /price). The override wins until explicitly cleared
//! (DELETE /price), so a staged crash holds still while the feed keeps ticking underneath.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use styx_core::consts::K_PAR;
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::oracle::{OracleSlot, TickPayload};
use styx_core::units::{BlockHeight, Price};
use styx_node::NodeError;
use styx_watch::quotes::WireQuote;
use styx_watch::transport::QuoteTransport;

use crate::feed::PriceSource;

/// Where the effective price currently comes from (surfaced by /health).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceMode {
    /// The sticky manual override.
    Override,
    /// A live backend delivered at least one update.
    Feed,
    /// The config's starting price; no backend update yet.
    Config,
}

impl PriceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PriceMode::Override => "override",
            PriceMode::Feed => "feed",
            PriceMode::Config => "config",
        }
    }
}

pub struct OracleState {
    slot: OracleSlot,
    keypair: zkp::Keypair,
    feed: RwLock<Price>,
    override_price: RwLock<Option<Price>>,
    feed_updated: RwLock<Option<Instant>>,
    /// The staleness gate: with a limit set, the oracle STOPS PUBLISHING once the backend
    /// has been silent past it (and before its first delivery). Silence degrades the
    /// quorum gracefully - 3-of-5 shrinking freezes the system into safe mode - while a
    /// frozen price re-signed under fresh heights is indistinguishable from a live one to
    /// the covenants. The trade-off (liveness loss against stale-price risk) is the
    /// operator's, hence opt-in; a sticky override always publishes.
    max_feed_age: Option<Duration>,
    /// The last height a quote was published for; 0 means none yet.
    pub last_published: AtomicU32,
}

impl OracleState {
    pub fn new(slot: OracleSlot, keypair: zkp::Keypair, initial: Price) -> Self {
        OracleState {
            slot,
            keypair,
            feed: RwLock::new(initial),
            override_price: RwLock::new(None),
            feed_updated: RwLock::new(None),
            max_feed_age: None,
            last_published: AtomicU32::new(0),
        }
    }

    /// Arm the staleness gate (builder-style, before the state goes behind an Arc).
    pub fn with_max_feed_age(mut self, limit: Option<Duration>) -> Self {
        self.max_feed_age = limit;
        self
    }

    /// May this oracle sign quotes right now? False only under an armed staleness gate
    /// with a quiet (or never-delivered) backend and no operator override.
    pub fn publishable(&self) -> bool {
        if self.override_price.read().unwrap_or_else(|e| e.into_inner()).is_some() {
            return true;
        }
        match self.max_feed_age {
            None => true,
            Some(limit) => self
                .feed_updated
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|at| at.elapsed() <= limit),
        }
    }

    pub fn slot(&self) -> OracleSlot {
        self.slot
    }

    /// The effective price: the sticky override when set, the feed otherwise.
    pub fn price(&self) -> Price {
        self.override_price
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .unwrap_or(*self.feed.read().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn mode(&self) -> PriceMode {
        if self.override_price.read().unwrap_or_else(|e| e.into_inner()).is_some() {
            PriceMode::Override
        } else if self.feed_updated.read().unwrap_or_else(|e| e.into_inner()).is_some() {
            PriceMode::Feed
        } else {
            PriceMode::Config
        }
    }

    /// Seconds since the backend last delivered, if it ever has. A growing number under
    /// `Feed` mode means the backend went quiet and the daemon keeps signing the last
    /// known price - watch it.
    pub fn feed_age_secs(&self) -> Option<u64> {
        self.feed_updated.read().unwrap_or_else(|e| e.into_inner()).map(|at| at.elapsed().as_secs())
    }

    /// A backend update: lands under the override, never displaces it.
    pub fn set_feed_price(&self, price: Price) {
        *self.feed.write().unwrap_or_else(|e| e.into_inner()) = price;
        *self.feed_updated.write().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    /// The sticky scenario override.
    pub fn set_override(&self, price: Price) {
        *self.override_price.write().unwrap_or_else(|e| e.into_inner()) = Some(price);
    }

    /// Clear the override; the feed (or config price) takes back over.
    pub fn clear_override(&self) {
        *self.override_price.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Sign the current price for a height. v1 publishes par backing (the reserve backstop
    /// phase computes a real backing_k later).
    pub fn quote_for(&self, height: BlockHeight) -> WireQuote {
        let payload = TickPayload { height, price: self.price(), backing_k: K_PAR };
        WireQuote::sign(&self.keypair, self.slot, &payload)
    }
}

/// The feed loop: poll the backend, land updates under the override. A failed poll keeps
/// the last price (the byzantine tolerance of the quorum is the real guard; /health's
/// feed age and the staleness gate are the operator's) and retries on the same cadence.
/// Outage logging is on-transition: one line when the backend goes quiet, one when it
/// recovers - not one per poll across an hour-long outage.
pub async fn run_feed(mut source: impl PriceSource, state: Arc<OracleState>, poll: Duration) {
    let mut failing = false;
    loop {
        match source.fetch().await {
            Ok(price) => {
                if failing {
                    eprintln!("feed: recovered at {}", price.raw());
                    failing = false;
                }
                state.set_feed_price(price);
            }
            Err(e) => {
                if !failing {
                    eprintln!("feed: {e} (keeping the last price; will log recovery)");
                    failing = true;
                }
            }
        }
        tokio::time::sleep(poll).await;
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
    let mut gated = false;
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
                    // The staleness gate: silence over a frozen price (see OracleState).
                    if !state.publishable() {
                        if !gated {
                            eprintln!(
                                "feed stale beyond the limit: withholding quotes \
                                 (the quorum degrades instead of freezing the price)"
                            );
                            gated = true;
                        }
                    } else {
                        if gated {
                            eprintln!("feed fresh again: publishing resumes");
                            gated = false;
                        }
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
        }
        tokio::time::sleep(poll).await;
    }
}
