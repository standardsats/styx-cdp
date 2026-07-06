//! The quotes core off transport: wire codec, verified caching, tick assembly - and the
//! loop closed against the covenants (an assembled tick finalizes a POKE at the prune tier).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::oracle::{OracleSlot, TickPayload};
use styx_core::units::{BlockHeight, Price, RatioK};
use styx_pset::build;
use styx_pset::finalize::finalize;
use styx_pset::testkit::{poke_intent, protocol_state, TestDeploy};
use styx_watch::quotes::{QuoteBook, QuoteError, WireQuote, DEFAULT_DRIFT, DEFAULT_HORIZON};

fn payload(height: u32, price: u32) -> TickPayload {
    TickPayload {
        height: BlockHeight::new(height),
        price: Price::new(price),
        backing_k: RatioK::from_cr_percent(100),
    }
}

/// A quote signed by the genuine oracle of `slot` (the testkit deploy's keys).
fn quote(d: &TestDeploy, slot: u8, height: u32, price: u32) -> WireQuote {
    WireQuote::sign(
        &d.oracle_keys[slot as usize],
        OracleSlot::new(slot).unwrap(),
        &payload(height, price),
    )
}

#[test]
fn wire_quotes_assemble_into_a_covenant_accepted_tick() {
    let d = TestDeploy::get();
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    let tip = BlockHeight::new(10);

    // Three quotes arrive over the wire (JSON round trip included), diverging in price.
    for (slot, price) in [(0u8, 120_000u32), (1, 119_000), (2, 121_000)] {
        let wire = WireQuote::from_json(&quote(d, slot, 10, price).to_json()).expect("json");
        book.insert(&wire, tip).expect("verifies");
    }
    assert_eq!(book.count(tip), 3);
    let tick = book.assemble_tick(tip).expect("quorum");
    assert_eq!(tick.height(), tip);
    assert_eq!(tick.price_range(), (Price::new(119_000), Price::new(121_000)));

    // The assembled tick is covenant-grade: a POKE built from it passes the prune tier.
    let protocol = protocol_state(1_000_000, 1_000_000, 5);
    let built = build::poke::poke(&d.ctx, &protocol.issuer, &poke_intent(tick)).expect("builds");
    finalize(&d.ctx, &built.plan).expect("prune accepts");
}

#[test]
fn the_book_rejects_what_the_covenant_would() {
    let d = TestDeploy::get();
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    let tip = BlockHeight::new(100);

    // A foreign signature: signed by oracle 1, claimed as slot 0.
    let mut foreign = quote(d, 1, 100, 120_000);
    foreign.slot = 0;
    assert!(matches!(
        book.insert(&foreign, tip),
        Err(QuoteError::BadSignature { slot: 0, height: 100 })
    ));

    // A slot outside the quorum shape.
    let mut bad_slot = quote(d, 0, 100, 120_000);
    bad_slot.slot = 7;
    assert!(matches!(book.insert(&bad_slot, tip), Err(QuoteError::Slot(7))));

    // A signature transplanted onto another height: the digest commits every field.
    let mut moved = quote(d, 0, 100, 120_000);
    moved.height = 101;
    assert!(matches!(book.insert(&moved, tip), Err(QuoteError::BadSignature { .. })));
    let mut repriced = quote(d, 0, 100, 120_000);
    repriced.price = 119_999;
    assert!(matches!(book.insert(&repriced, tip), Err(QuoteError::BadSignature { .. })));

    // Stale and future heights against the tip window.
    let stale = quote(d, 0, 100 - DEFAULT_HORIZON - 1, 120_000);
    assert!(matches!(book.insert(&stale, tip), Err(QuoteError::Stale { .. })));
    let future = quote(d, 0, 100 + DEFAULT_DRIFT + 1, 120_000);
    assert!(matches!(book.insert(&future, tip), Err(QuoteError::Future { .. })));

    // Malformed signature encoding.
    let mut garbled = quote(d, 0, 100, 120_000);
    garbled.sig.truncate(10);
    assert!(matches!(book.insert(&garbled, tip), Err(QuoteError::SigEncoding)));

    assert_eq!(book.count(tip), 0, "no rejected quote leaves a trace");
}

#[test]
fn assembly_needs_three_distinct_slots_sharing_a_backing_k() {
    let d = TestDeploy::get();
    let tip = BlockHeight::new(50);
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);

    book.insert(&quote(d, 0, 50, 120_000), tip).unwrap();
    book.insert(&quote(d, 1, 50, 120_000), tip).unwrap();
    assert!(book.assemble_tick(tip).is_none(), "two quotes are below quorum");
    // A duplicate from the same slot replaces, never widens the quorum.
    book.insert(&quote(d, 1, 50, 120_500), tip).unwrap();
    assert!(book.assemble_tick(tip).is_none());
    book.insert(&quote(d, 2, 50, 121_000), tip).unwrap();
    let tick = book.assemble_tick(tip).expect("quorum reached");
    assert_eq!(tick.slots().iter().flatten().count(), 3);

    // Diverging backing_k: only a k shared by three slots can form a tick.
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    let under = RatioK::from_cr_percent(90);
    for slot in [0u8, 1] {
        book.insert(&quote(d, slot, 50, 120_000), tip).unwrap();
    }
    for slot in [2u8, 3, 4] {
        let p =
            TickPayload { height: BlockHeight::new(50), price: Price::new(120_000), backing_k: under };
        let wire = WireQuote::sign(&d.oracle_keys[slot as usize], OracleSlot::new(slot).unwrap(), &p);
        book.insert(&wire, tip).unwrap();
    }
    let tick = book.assemble_tick(tip).expect("the under-backed k has three slots");
    assert_eq!(tick.backing_k(), under);
}

#[tokio::test]
async fn the_mock_hub_delivers_quotes_between_endpoints() {
    use styx_watch::transport::{MockHub, QuoteTransport};

    let d = TestDeploy::get();
    let hub = MockHub::new();
    let mut oracle_side = hub.endpoint();
    let mut consumer = hub.endpoint();

    for slot in [0u8, 1, 2] {
        oracle_side.publish(&quote(d, slot, 7, 120_000)).await.expect("publish");
    }
    let tip = BlockHeight::new(7);
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    for _ in 0..3 {
        let q = consumer.recv().await.expect("recv");
        book.insert(&q, tip).expect("verifies");
    }
    assert!(book.assemble_tick(tip).is_some());
}

/// A validly signed zero price is covenant-invalid and must never enter the book: assembly
/// picks slots blindly, so one byzantine oracle would otherwise poison the tick at every
/// height - 3-of-5 is supposed to survive two byzantine slots.
#[test]
fn a_signed_zero_price_is_rejected_at_insert() {
    let d = TestDeploy::get();
    let mut book = QuoteBook::new(d.ctx.params.oracle_pks);
    let tip = BlockHeight::new(10);

    let zero = quote(d, 0, 10, 0);
    assert!(matches!(book.insert(&zero, tip), Err(QuoteError::ZeroPrice { slot: 0, height: 10 })));
    assert_eq!(book.count(tip), 0);

    // The honest majority still assembles around the poisoned slot.
    for slot in [1u8, 2, 3] {
        book.insert(&quote(d, slot, 10, 120_000), tip).unwrap();
    }
    let (lo, _) = book.assemble_tick(tip).expect("quorum").price_range();
    assert_eq!(lo, Price::new(120_000));
}
