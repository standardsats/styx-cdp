//! The oracle daemon off node and off relay: the signing path against the verified book,
//! the block loop over the mock hub, and the admin HTTP surface driven through the router
//! (tower oneshot - the same code `main` serves).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::oracle::OracleSlot;
use styx_core::units::{BlockHeight, Price};
use styx_oracle::http::router;
use styx_oracle::service::{publish_blocks, OracleState};
use styx_watch::quotes::{QuoteBook, WireQuote};
use styx_watch::transport::{MockHub, QuoteTransport};
use tower::ServiceExt;

fn keypair(secret: u8) -> zkp::Keypair {
    let mut sk = [0u8; 32];
    sk[31] = secret;
    zkp::Keypair::from_seckey_slice(styx_core::secp(), &sk).unwrap()
}

/// The five covenant oracle keys with our daemon at slot 0.
fn oracle_pks() -> [XOnlyPublicKey; 5] {
    [7u8, 8, 9, 101, 102].map(|s| keypair(s).x_only_public_key().0)
}

fn state(price: u32) -> Arc<OracleState> {
    Arc::new(OracleState::new(OracleSlot::new(0).unwrap(), keypair(7), Price::new(price)))
}

#[test]
fn a_daemon_quote_verifies_in_the_book() {
    let s = state(120_000);
    let q = s.quote_for(BlockHeight::new(42));
    let mut book = QuoteBook::new(oracle_pks());
    book.insert(&q, BlockHeight::new(42)).expect("verifies against slot 0");
    assert_eq!(q.price, 120_000);
    assert_eq!(q.backing_k, styx_core::consts::K_PAR.raw());
}

#[tokio::test]
async fn the_block_loop_publishes_once_per_new_tip() {
    let s = state(120_000);
    let hub = MockHub::new();
    let mut consumer = hub.endpoint();
    let mut oracle_side = hub.endpoint();

    let tip = Arc::new(AtomicU32::new(5));
    let tip_for_loop = tip.clone();
    let loop_state = s.clone();
    let task = tokio::spawn(async move {
        let height = move || Ok(tip_for_loop.load(Ordering::SeqCst));
        publish_blocks(
            loop_state,
            &mut oracle_side,
            height,
            Duration::from_millis(10),
            Duration::from_secs(60),
        )
        .await
    });

    // The starting tip is published once, then each bump yields exactly one quote for the
    // new tip (an unchanged tip publishes nothing in between).
    let q = tokio::time::timeout(Duration::from_secs(5), consumer.recv()).await.unwrap().unwrap();
    assert_eq!(q.height, 5);
    tokio::time::sleep(Duration::from_millis(50)).await;
    tip.store(8, Ordering::SeqCst);
    let q = tokio::time::timeout(Duration::from_secs(5), consumer.recv()).await.unwrap().unwrap();
    assert_eq!(q.height, 8, "one quote for the tip, no backfill of skipped heights");
    assert_eq!(s.last_published.load(Ordering::SeqCst), 8);
    task.abort();
}

async fn body_json<T: serde::de::DeserializeOwned>(resp: axum::response::Response) -> T {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn the_admin_surface_moves_the_price_and_serves_signed_quotes() {
    let s = state(120_000);
    let app = router(s.clone());

    // /health reflects the slot and the starting price.
    let resp = app.clone().oneshot(Request::get("/health").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let health: serde_json::Value = body_json(resp).await;
    assert_eq!(health["slot"], 0);
    assert_eq!(health["price"], 120_000);

    // The scenario lever: crash the price.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/price")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"usd":50000}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(s.price(), Price::new(50_000));

    // /quote signs the moved price on demand, and the signature is covenant-grade.
    let resp = app
        .clone()
        .oneshot(Request::get("/quote?height=99").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let quote: WireQuote = body_json(resp).await;
    assert_eq!(quote.height, 99);
    assert_eq!(quote.price, 50_000);
    let mut book = QuoteBook::new(oracle_pks());
    book.insert(&quote, BlockHeight::new(99)).expect("verifies");

    // A zero price is never signed.
    let resp = app
        .oneshot(
            Request::post("/price")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"usd":0}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(s.price(), Price::new(50_000));
}

/// A relay hiccup is not fatal: the publish is retried for the same height on the next poll
/// (last_published only advances on success).
#[tokio::test]
async fn a_transport_hiccup_retries_the_same_height() {
    struct Flaky {
        inner: styx_watch::transport::MockTransport,
        fail_first: bool,
    }
    impl QuoteTransport for Flaky {
        async fn publish(&mut self, q: &WireQuote) -> Result<(), styx_watch::transport::TransportError> {
            if self.fail_first {
                self.fail_first = false;
                return Err(styx_watch::transport::TransportError::Backend("relay down".into()));
            }
            self.inner.publish(q).await
        }
        async fn recv(&mut self) -> Result<WireQuote, styx_watch::transport::TransportError> {
            self.inner.recv().await
        }
    }

    let s = state(120_000);
    let hub = MockHub::new();
    let mut consumer = hub.endpoint();
    let mut flaky = Flaky { inner: hub.endpoint(), fail_first: true };
    let loop_state = s.clone();
    let task = tokio::spawn(async move {
        publish_blocks(
            loop_state,
            &mut flaky,
            move || Ok(5),
            Duration::from_millis(10),
            Duration::from_secs(60),
        )
        .await
    });

    // The first attempt fails; the retry lands the same height.
    let q = tokio::time::timeout(Duration::from_secs(5), consumer.recv()).await.unwrap().unwrap();
    assert_eq!(q.height, 5);
    assert_eq!(s.last_published.load(Ordering::SeqCst), 5);
    task.abort();
}

/// A node outage is tolerated up to the deadline, then fatal.
#[tokio::test]
async fn a_persistent_node_outage_is_fatal_after_the_deadline() {
    use styx_node::NodeError;
    use styx_oracle::service::RunError;

    let s = state(120_000);
    let hub = MockHub::new();
    let mut oracle_side = hub.endpoint();
    let height = move || -> Result<u32, NodeError> {
        Err(NodeError::Rpc { method: "getblockcount".into(), message: "connection refused".into() })
    };
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        publish_blocks(
            s,
            &mut oracle_side,
            height,
            Duration::from_millis(10),
            Duration::from_millis(50),
        ),
    )
    .await
    .expect("dies before the timeout")
    .expect_err("must be fatal");
    assert!(matches!(err, RunError::NodeDown { .. }));
}

/// The config twin of the zero-price refusals downstream.
#[test]
fn a_zero_starting_price_fails_config_parse() {
    use styx_oracle::config::{ConfigError, OracleConfig};

    let toml = |price: u32| {
        format!(
            r#"
slot = 0
protocol_seckey = "{}"
nostr_seckey = "{}"
relays = ["ws://127.0.0.1:8080"]
rpc_url = "http://127.0.0.1:7040"
rpc_user = "styx"
rpc_password = "styx"
listen = "127.0.0.1:9700"
price_usd = {price}
"#,
            "07".repeat(32),
            "08".repeat(32),
        )
    };
    assert!(matches!(OracleConfig::parse(&toml(0)), Err(ConfigError::ZeroPrice)));
    assert_eq!(OracleConfig::parse(&toml(120_000)).expect("parses").price_usd, 120_000);
}

/// The live feed path against a local mock exchange: the state follows the backend, a
/// sticky override pins the price while the feed keeps ticking underneath, and clearing
/// the override hands the price back to the feed.
#[tokio::test]
async fn the_feed_follows_the_exchange_and_yields_to_the_override() {
    use styx_oracle::feed::{Backend, HttpFeed};
    use styx_oracle::service::{run_feed, PriceMode};

    // A Coinbase-shaped mock exchange with a movable price.
    let quoted = Arc::new(AtomicU32::new(120_000));
    let mock = {
        let quoted = quoted.clone();
        axum::Router::new().route(
            "/spot",
            axum::routing::get(move || {
                let quoted = quoted.clone();
                async move {
                    format!(
                        r#"{{"data":{{"amount":"{}.00","base":"BTC","currency":"USD"}}}}"#,
                        quoted.load(Ordering::SeqCst)
                    )
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/spot", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock).await;
    });

    let s = state(1); // config price 1: the feed must displace it immediately
    let source = HttpFeed::new(Backend::Coinbase, Some(url)).unwrap();
    tokio::spawn(run_feed(source, s.clone(), Duration::from_millis(25)));

    let wait_price = |s: Arc<styx_oracle::service::OracleState>, want: u32| async move {
        for _ in 0..200 {
            if s.price() == Price::new(want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("price never reached {want} (at {})", s.price().raw());
    };

    // The backend's price lands and /health calls it a feed.
    wait_price(s.clone(), 120_000).await;
    assert_eq!(s.mode(), PriceMode::Feed);
    assert!(s.feed_age_secs().is_some());

    // The exchange moves; the oracle follows.
    quoted.store(118_000, Ordering::SeqCst);
    wait_price(s.clone(), 118_000).await;

    // A staged crash pins the price regardless of the live feed underneath.
    s.set_override(Price::new(50_000));
    assert_eq!(s.price(), Price::new(50_000));
    assert_eq!(s.mode(), PriceMode::Override);
    quoted.store(119_000, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.price(), Price::new(50_000), "the override is sticky");

    // Clearing it hands the price back to the (still ticking) feed.
    s.clear_override();
    wait_price(s.clone(), 119_000).await;
    assert_eq!(s.mode(), PriceMode::Feed);
}

/// The staleness gate: with max_feed_age armed, the oracle withholds quotes until the
/// backend delivers and again once it goes quiet - silence degrades the quorum into a
/// safe freeze, a frozen price would not. The operator's override publishes regardless.
#[tokio::test]
async fn the_staleness_gate_withholds_quotes_when_the_feed_goes_quiet() {
    let s = Arc::new(
        styx_oracle::service::OracleState::new(
            OracleSlot::new(0).unwrap(),
            keypair(7),
            Price::new(120_000),
        )
        .with_max_feed_age(Some(Duration::from_millis(80))),
    );

    // Before the first delivery: gated (publishing the config price would BE the frozen
    // price the gate exists to prevent).
    assert!(!s.publishable());

    // A delivery opens the gate; the block loop confirms by actually publishing.
    s.set_feed_price(Price::new(121_000));
    assert!(s.publishable());
    let hub = MockHub::new();
    let mut consumer = hub.endpoint();
    let mut oracle_side = hub.endpoint();
    let loop_state = s.clone();
    let task = tokio::spawn(async move {
        publish_blocks(
            loop_state,
            &mut oracle_side,
            move || Ok(5),
            Duration::from_millis(10),
            Duration::from_secs(60),
        )
        .await
    });
    let q = tokio::time::timeout(Duration::from_secs(5), consumer.recv()).await.unwrap().unwrap();
    assert_eq!(q.price, 121_000);
    task.abort();

    // The backend goes quiet past the limit: gated again...
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(!s.publishable(), "silent past max_feed_age");

    // ...unless the operator pins an override (scenario control beats a dead feed).
    s.set_override(Price::new(50_000));
    assert!(s.publishable());
    s.clear_override();
    assert!(!s.publishable());

    // A fresh delivery reopens it.
    s.set_feed_price(Price::new(119_000));
    assert!(s.publishable());
}
