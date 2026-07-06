//! Nostr transport for oracle quotes.
//!
//! One quote is one addressable event (NIP-01 parameterized-replaceable, kind in the 3xxxx
//! range) with `d` = the block height, so a relay keeps exactly the latest quote per
//! (oracle, height) and a consumer gets at most five events per height from a single
//! `authors + kind + d` filter. Replacement needs a strictly growing `created_at`, so the
//! publisher bumps it explicitly when publishing faster than the clock ticks. NIP-40
//! expiration is attached as a GC hint - the quorum is keyed by height, staleness never
//! rests on the relay honoring it.
//!
//! The Nostr identity of an oracle is transport-only: consumers filter by the configured
//! authors to keep spam out, but every quote is verified against the covenant oracle keys
//! (`QuoteBook::insert`) regardless of who relayed it.

use nostr_sdk::prelude::*;

use crate::quotes::WireQuote;
use crate::transport::{QuoteTransport, TransportError};

/// The addressable kind for STYX oracle quotes.
pub const QUOTE_KIND: Kind = Kind::Custom(33321);

/// NIP-40 expiration horizon: long enough for any consumer to catch up, short enough that
/// relays do not accumulate the whole history.
const EXPIRY_SECS: u64 = 3_600;

fn backend(e: impl std::fmt::Display) -> TransportError {
    TransportError::Backend(e.to_string())
}

/// A connected Nostr client in one of two roles: an oracle publishing under its transport
/// keys, or a consumer subscribed to the five oracle authors.
pub struct NostrQuotes {
    client: Client,
    notifications: tokio::sync::broadcast::Receiver<RelayPoolNotification>,
    authors: Vec<PublicKey>,
    last_created_at: Timestamp,
}

impl NostrQuotes {
    /// An oracle's endpoint: publishes under `keys`. It does not subscribe, and it must not
    /// call `recv`: with no author list, the empty-filter convention in `parse` would accept
    /// any well-formed event should one ever reach the notification stream.
    pub async fn publisher(relays: &[String], keys: Keys) -> Result<Self, TransportError> {
        let client = Client::new(keys);
        Self::connect(client, relays, Vec::new()).await
    }

    /// A consumer's endpoint: ephemeral transport identity, subscribed to the oracle
    /// authors (slot -> nostr pubkey from styxnet.toml).
    pub async fn subscriber(relays: &[String], authors: Vec<PublicKey>) -> Result<Self, TransportError> {
        let client = Client::new(Keys::generate());
        let this = Self::connect(client, relays, authors).await?;
        let filter = Filter::new().authors(this.authors.iter().copied()).kind(QUOTE_KIND);
        this.client.subscribe(filter, None).await.map_err(backend)?;
        Ok(this)
    }

    async fn connect(
        client: Client,
        relays: &[String],
        authors: Vec<PublicKey>,
    ) -> Result<Self, TransportError> {
        for url in relays {
            client.add_relay(url.clone()).await.map_err(backend)?;
        }
        client.connect().await;
        let notifications = client.notifications();
        Ok(NostrQuotes { client, notifications, authors, last_created_at: Timestamp::zero() })
    }

    /// One-shot fetch of the stored quotes for a height (the `authors + kind + d` filter):
    /// the catch-up path, complementing the live subscription.
    pub async fn fetch(
        &self,
        height: u32,
        timeout: std::time::Duration,
    ) -> Result<Vec<WireQuote>, TransportError> {
        let filter = Filter::new()
            .authors(self.authors.iter().copied())
            .kind(QUOTE_KIND)
            .identifier(height.to_string());
        let events = self.client.fetch_events(filter, timeout).await.map_err(backend)?;
        Ok(events.into_iter().filter_map(|e| Self::parse(&e, &self.authors)).collect())
    }

    fn parse(event: &Event, authors: &[PublicKey]) -> Option<WireQuote> {
        if event.kind != QUOTE_KIND {
            return None;
        }
        if !authors.is_empty() && !authors.contains(&event.pubkey) {
            return None;
        }
        let quote = WireQuote::from_json(&event.content).ok()?;
        // The d tag is replacement bookkeeping; a mismatch with the payload means a
        // malformed publisher, and the event is dropped rather than trusted either way.
        if event.tags.identifier() != Some(quote.height.to_string().as_str()) {
            return None;
        }
        Some(quote)
    }
}

impl QuoteTransport for NostrQuotes {
    async fn publish(&mut self, quote: &WireQuote) -> Result<(), TransportError> {
        // Replacement semantics need a strictly growing created_at per (author, kind, d);
        // publishing twice within one second would otherwise be relay-order-dependent.
        let now = Timestamp::now();
        let created_at = if now > self.last_created_at { now } else { self.last_created_at + 1u64 };
        self.last_created_at = created_at;
        let builder = EventBuilder::new(QUOTE_KIND, quote.to_json())
            .tags([Tag::identifier(quote.height.to_string()), Tag::expiration(created_at + EXPIRY_SECS)])
            .custom_created_at(created_at);
        self.client.send_event_builder(builder).await.map_err(backend)?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<WireQuote, TransportError> {
        loop {
            match self.notifications.recv().await {
                Ok(RelayPoolNotification::Event { event, .. }) => {
                    if let Some(q) = Self::parse(&event, &self.authors) {
                        return Ok(q);
                    }
                }
                Ok(_) => continue,
                // Lagging silently drops events, which is safe for quotes: the book only
                // needs the freshest heights, and a gap is recoverable via `fetch`.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(TransportError::Closed)
                }
            }
        }
    }
}
