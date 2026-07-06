//! The price backend abstraction and the exchange implementations.
//!
//! Every oracle slot runs its OWN backend (one exchange per slot on a real deployment), so
//! the quorum's min/max divergence reflects genuinely independent sources. A backend only
//! needs to answer "what is BTC/USD right now": the daemon polls it every few seconds,
//! which is far below the staleness that matters at one quote per block - and makes
//! reconnection free (every poll is a fresh request). A streaming backend (websocket)
//! would implement the same trait with `fetch` returning the next pushed update.
//!
//! Parsers are pure and unit-tested against canned response bodies; the network never
//! enters the fast tier.

use styx_core::units::Price;

#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("http: {0}")]
    Http(String),
    #[error("{backend}: unexpected response shape: {detail}")]
    Shape { backend: &'static str, detail: String },
    #[error("{backend}: price {price} outside the sane range")]
    Insane { backend: &'static str, price: f64 },
}

/// A price backend: where an oracle's USD/BTC comes from.
pub trait PriceSource: Send {
    fn fetch(&mut self) -> impl std::future::Future<Output = Result<Price, FeedError>> + Send;
}

/// The supported exchanges (public spot tickers, no keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Coinbase,
    /// BTC/USDT: a dollar proxy, like every USDT pair.
    Binance,
    Kraken,
    Bitstamp,
    Bitfinex,
}

impl std::str::FromStr for Backend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "coinbase" => Ok(Backend::Coinbase),
            "binance" => Ok(Backend::Binance),
            "kraken" => Ok(Backend::Kraken),
            "bitstamp" => Ok(Backend::Bitstamp),
            "bitfinex" => Ok(Backend::Bitfinex),
            other => Err(format!(
                "unknown feed backend {other} (coinbase / binance / kraken / bitstamp / bitfinex)"
            )),
        }
    }
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Coinbase => "coinbase",
            Backend::Binance => "binance",
            Backend::Kraken => "kraken",
            Backend::Bitstamp => "bitstamp",
            Backend::Bitfinex => "bitfinex",
        }
    }

    /// The public spot-ticker endpoint.
    pub fn endpoint(&self) -> &'static str {
        match self {
            Backend::Coinbase => "https://api.coinbase.com/v2/prices/BTC-USD/spot",
            Backend::Binance => "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT",
            Backend::Kraken => "https://api.kraken.com/0/public/Ticker?pair=XBTUSD",
            Backend::Bitstamp => "https://www.bitstamp.net/api/v2/ticker/btcusd/",
            Backend::Bitfinex => "https://api-pub.bitfinex.com/v2/ticker/tBTCUSD",
        }
    }

    /// Parse the exchange's response body into an integer USD price. Pure: the whole
    /// per-exchange quirk surface lives here, under fast-tier tests.
    pub fn parse_price(&self, body: &str) -> Result<Price, FeedError> {
        let shape = |detail: &str| FeedError::Shape { backend: self.name(), detail: detail.into() };
        let json = || -> Result<serde_json::Value, FeedError> {
            serde_json::from_str(body).map_err(|e| shape(&e.to_string()))
        };
        let raw: f64 = match self {
            Backend::Coinbase => json()?["data"]["amount"]
                .as_str()
                .ok_or_else(|| shape("data.amount missing"))?
                .parse()
                .map_err(|_| shape("data.amount not a number"))?,
            Backend::Binance => json()?["price"]
                .as_str()
                .ok_or_else(|| shape("price missing"))?
                .parse()
                .map_err(|_| shape("price not a number"))?,
            Backend::Kraken => {
                let v = json()?;
                let result = v["result"].as_object().ok_or_else(|| shape("result missing"))?;
                // The pair key varies ("XXBTZUSD"); take the single entry.
                let (_, pair) = result.iter().next().ok_or_else(|| shape("result empty"))?;
                pair["c"][0]
                    .as_str()
                    .ok_or_else(|| shape("result.*.c[0] missing"))?
                    .parse()
                    .map_err(|_| shape("c[0] not a number"))?
            }
            Backend::Bitstamp => json()?["last"]
                .as_str()
                .ok_or_else(|| shape("last missing"))?
                .parse()
                .map_err(|_| shape("last not a number"))?,
            // An array ticker: [bid, bid_size, ask, ask_size, chg, chg_rel, LAST, vol, hi, lo]
            Backend::Bitfinex => json()?[6].as_f64().ok_or_else(|| shape("ticker[6] missing"))?,
        };
        sane(self.name(), raw)
    }
}

/// The covenants carry prices as integer USD; anything outside (0, 100M) per BTC is a
/// broken feed, not a market.
const MAX_SANE_PRICE: f64 = 100_000_000.0;

fn sane(backend: &'static str, raw: f64) -> Result<Price, FeedError> {
    if !raw.is_finite() || !(1.0..MAX_SANE_PRICE).contains(&raw) {
        return Err(FeedError::Insane { backend, price: raw });
    }
    Ok(Price::new(raw.round() as u32))
}

/// A polled HTTP backend. `url` is configurable so tests (and closed networks) can point
/// an exchange parser at a local mock.
pub struct HttpFeed {
    pub backend: Backend,
    url: String,
    client: reqwest::Client,
}

impl HttpFeed {
    pub fn new(backend: Backend, url: Option<String>) -> Result<Self, FeedError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("styx-oracle/0.1")
            .build()
            .map_err(|e| FeedError::Http(e.to_string()))?;
        Ok(HttpFeed { backend, url: url.unwrap_or_else(|| backend.endpoint().to_string()), client })
    }
}

impl PriceSource for HttpFeed {
    async fn fetch(&mut self) -> Result<Price, FeedError> {
        let body = self
            .client
            .get(&self.url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| FeedError::Http(e.to_string()))?
            .text()
            .await
            .map_err(|e| FeedError::Http(e.to_string()))?;
        self.backend.parse_price(&body)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_exchange_shape_parses() {
        let cases: [(Backend, &str, u32); 5] = [
            (
                Backend::Coinbase,
                r#"{"data":{"amount":"104250.335","base":"BTC","currency":"USD"}}"#,
                104250,
            ),
            (Backend::Binance, r#"{"symbol":"BTCUSDT","price":"104250.99000000"}"#, 104251),
            (
                Backend::Kraken,
                r#"{"error":[],"result":{"XXBTZUSD":{"a":["104300.0","1","1.0"],"b":["104200.0","2","2.0"],"c":["104250.5","0.012"],"v":["100","200"]}}}"#,
                104251,
            ),
            (
                Backend::Bitstamp,
                r#"{"timestamp":"1751800000","last":"104249.50","high":"105000","low":"103000"}"#,
                104250,
            ),
            (
                Backend::Bitfinex,
                r#"[104200.0,5.5,104300.0,4.2,-100.0,-0.001,104250.0,1234.5,105000.0,103000.0]"#,
                104250,
            ),
        ];
        for (backend, body, want) in cases {
            assert_eq!(
                backend.parse_price(body).unwrap(),
                styx_core::units::Price::new(want),
                "{}",
                backend.name()
            );
        }
    }

    #[test]
    fn garbage_and_insane_prices_are_refused() {
        assert!(matches!(
            Backend::Coinbase.parse_price("not json"),
            Err(FeedError::Shape { backend: "coinbase", .. })
        ));
        assert!(matches!(
            Backend::Binance.parse_price(r#"{"code":-1121,"msg":"Invalid symbol."}"#),
            Err(FeedError::Shape { .. })
        ));
        assert!(matches!(
            Backend::Coinbase.parse_price(r#"{"data":{"amount":"0"}}"#),
            Err(FeedError::Insane { .. })
        ));
        assert!(matches!(
            Backend::Coinbase.parse_price(r#"{"data":{"amount":"999999999999"}}"#),
            Err(FeedError::Insane { .. })
        ));
        // Kraken reports failures inside a 200 body.
        assert!(matches!(
            Backend::Kraken.parse_price(r#"{"error":["EQuery:Unknown asset pair"]}"#),
            Err(FeedError::Shape { .. })
        ));
    }

    #[test]
    fn backend_names_round_trip() {
        for name in ["coinbase", "binance", "kraken", "bitstamp", "bitfinex"] {
            let b: Backend = name.parse().unwrap();
            assert_eq!(b.name(), name);
        }
        assert!("mtgox".parse::<Backend>().is_err());
    }
}
