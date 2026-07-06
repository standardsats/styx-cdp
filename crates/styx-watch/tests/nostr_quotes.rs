//! The Nostr transport against an embedded relay (nostr-relay-builder's LocalRelay): five
//! oracles publish addressable quote events, a consumer assembles a tick from the live
//! stream and from the stored-events fetch, and a re-publication replaces rather than
//! accumulates.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use nostr_relay_builder::prelude::*;
use styx_core::oracle::{OracleSlot, TickPayload};
use styx_core::units::{BlockHeight, Price, RatioK};
use styx_pset::testkit::TestDeploy;
use styx_watch::nostr::NostrQuotes;
use styx_watch::quotes::{QuoteBook, WireQuote};
use styx_watch::transport::QuoteTransport;

fn wire(d: &TestDeploy, slot: u8, height: u32, price: u32) -> WireQuote {
    WireQuote::sign(
        &d.oracle_keys[slot as usize],
        OracleSlot::new(slot).unwrap(),
        &TickPayload {
            height: BlockHeight::new(height),
            price: Price::new(price),
            backing_k: RatioK::from_cr_percent(100),
        },
    )
}

#[tokio::test]
async fn five_oracles_one_relay_stream_fetch_and_replacement() {
    let d = TestDeploy::get();
    let relay = LocalRelay::new(RelayBuilder::default());
    relay.run().await.expect("local relay");
    let relays = vec![relay.url().await.to_string()];

    // Five oracle daemons, each with its own transport identity.
    let nostr_keys: Vec<Keys> = (0..5).map(|_| Keys::generate()).collect();
    let authors: Vec<PublicKey> = nostr_keys.iter().map(|k| k.public_key()).collect();
    let mut publishers = Vec::new();
    for keys in &nostr_keys {
        publishers.push(NostrQuotes::publisher(&relays, keys.clone()).await.expect("publisher"));
    }

    // The consumer subscribes first, then all five publish for height 42.
    let mut consumer = NostrQuotes::subscriber(&relays, authors.clone()).await.expect("subscriber");
    for (slot, publisher) in publishers.iter_mut().enumerate() {
        publisher.publish(&wire(d, slot as u8, 42, 120_000 + slot as u32)).await.expect("publish");
    }

    // Live stream: five quotes arrive, verify, and assemble.
    let tip = BlockHeight::new(42);
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    for _ in 0..5 {
        let q = tokio::time::timeout(Duration::from_secs(10), consumer.recv())
            .await
            .expect("stream delivers within the timeout")
            .expect("recv");
        book.insert(&q, tip).expect("verifies");
    }
    assert_eq!(book.count(tip), 5);
    let tick = book.assemble_tick(tip).expect("quorum");
    assert_eq!(tick.height(), tip);

    // Stored-events fetch (the authors + kind + d filter): a late consumer catches up.
    let late = NostrQuotes::subscriber(&relays, authors.clone()).await.expect("late subscriber");
    let fetched = late.fetch(42, Duration::from_secs(5)).await.expect("fetch");
    assert_eq!(fetched.len(), 5, "one stored event per oracle");

    // Replacement: oracle 0 re-publishes height 42 at a corrected price. The addressable
    // kind + the explicit created_at bump make the relay keep exactly the newer one.
    publishers[0].publish(&wire(d, 0, 42, 90_000)).await.expect("republish");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let fetched = late.fetch(42, Duration::from_secs(5)).await.expect("fetch after replace");
    assert_eq!(fetched.len(), 5, "replaced, not accumulated");
    let slot0: Vec<&WireQuote> = fetched.iter().filter(|q| q.slot == 0).collect();
    assert_eq!(slot0.len(), 1);
    assert_eq!(slot0[0].price, 90_000);

    // The replacement still verifies and re-assembles at the corrected price.
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    for q in &fetched {
        book.insert(q, tip).expect("verifies");
    }
    let (lo, _) = book.assemble_tick(tip).expect("quorum").price_range();
    assert_eq!(lo, Price::new(90_000));
}

#[tokio::test]
async fn a_stranger_on_the_relay_cannot_reach_the_book() {
    let d = TestDeploy::get();
    let relay = LocalRelay::new(RelayBuilder::default());
    relay.run().await.expect("local relay");
    let relays = vec![relay.url().await.to_string()];

    let oracle_keys = Keys::generate();
    let authors = vec![oracle_keys.public_key()];
    let mut consumer = NostrQuotes::subscriber(&relays, authors.clone()).await.expect("subscriber");

    // A stranger publishes a well-formed quote event under an unknown author: the consumer's
    // author filter drops it. (Even if it arrived, the book's BIP340 check is the real gate.)
    let mut stranger = NostrQuotes::publisher(&relays, Keys::generate()).await.expect("stranger");
    stranger.publish(&wire(d, 0, 7, 1)).await.expect("publish");

    // The genuine oracle publishes after; the first thing the consumer sees is genuine.
    let mut genuine = NostrQuotes::publisher(&relays, oracle_keys).await.expect("genuine");
    genuine.publish(&wire(d, 0, 7, 120_000)).await.expect("publish");

    let q = tokio::time::timeout(Duration::from_secs(10), consumer.recv())
        .await
        .expect("delivered")
        .expect("recv");
    assert_eq!(q.price, 120_000);
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    book.insert(&q, BlockHeight::new(7)).expect("verifies");
}
