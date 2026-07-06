//! The quote transport seam: how signed quotes travel between the oracle daemons and their
//! consumers. The protocol trust lives entirely in the quotes' BIP340 signatures
//! (`quotes::QuoteBook` verifies before caching), so a transport only needs delivery, not
//! integrity: Nostr in production (`nostr::NostrQuotes`), an in-memory hub in tests and
//! single-host smokes.

use crate::quotes::WireQuote;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport backend: {0}")]
    Backend(String),
    #[error("transport closed")]
    Closed,
}

/// Publish own quotes, receive everyone's. `recv` resolves once per incoming quote;
/// malformed events are dropped inside the implementation, so what comes out is always a
/// parsed `WireQuote` (still unverified - the book is the judge).
pub trait QuoteTransport {
    fn publish(
        &mut self,
        quote: &WireQuote,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;
    fn recv(&mut self) -> impl std::future::Future<Output = Result<WireQuote, TransportError>> + Send;
}

/// The in-memory hub: every endpoint sees every published quote (its own included, like a
/// relay echoing a subscription that matches the publisher).
pub struct MockHub {
    tx: tokio::sync::broadcast::Sender<WireQuote>,
}

impl Default for MockHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MockHub {
    pub fn new() -> Self {
        MockHub { tx: tokio::sync::broadcast::channel(256).0 }
    }

    pub fn endpoint(&self) -> MockTransport {
        MockTransport { tx: self.tx.clone(), rx: self.tx.subscribe() }
    }
}

pub struct MockTransport {
    tx: tokio::sync::broadcast::Sender<WireQuote>,
    rx: tokio::sync::broadcast::Receiver<WireQuote>,
}

impl QuoteTransport for MockTransport {
    async fn publish(&mut self, quote: &WireQuote) -> Result<(), TransportError> {
        // Every endpoint holds a receiver, so a send can only fail if the hub is gone.
        self.tx.send(quote.clone()).map(|_| ()).map_err(|_| TransportError::Closed)
    }

    async fn recv(&mut self) -> Result<WireQuote, TransportError> {
        loop {
            match self.rx.recv().await {
                Ok(q) => return Ok(q),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(TransportError::Closed)
                }
            }
        }
    }
}
