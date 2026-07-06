//! The network tier: every exchange backend against its REAL endpoint. A refused
//! connection is an environment fact and skips; a parse failure is a parser-reality
//! mismatch and fails - this is the test to run before a deployment and on any exchange
//! API grumble. At least one backend must validate for the run to count.
//!
//! Ignored by default (needs open egress to the exchanges):
//! `cargo test -p styx-oracle --test live_feeds -- --ignored --nocapture`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_oracle::feed::{Backend, FeedError, HttpFeed, PriceSource};

#[tokio::test]
#[ignore = "needs egress to the public exchanges"]
async fn every_reachable_exchange_parses_today() {
    let backends =
        [Backend::Coinbase, Backend::Binance, Backend::Kraken, Backend::Bitstamp, Backend::Bitfinex];
    let mut validated = 0;
    for backend in backends {
        let mut feed = HttpFeed::new(backend, None).unwrap();
        match feed.fetch().await {
            Ok(price) => {
                println!("{}: {} USD/BTC", backend.name(), price.raw());
                validated += 1;
            }
            Err(FeedError::Http(e)) => {
                println!("{}: SKIPPED (unreachable here): {e}", backend.name());
            }
            Err(e) => panic!("{}: the live API no longer matches our parser: {e}", backend.name()),
        }
    }
    assert!(validated > 0, "no exchange reachable: the run validated nothing");
    println!("validated {validated}/5 backends");
}
