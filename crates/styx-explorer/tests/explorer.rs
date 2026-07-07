//! The U2 invariants, off node: the dependency graph carries no signing crate (the M0
//! crate-graph rule as a ratchet), the page classifies bands the way the keeper ladder
//! does, and the served HTML references nothing external unless the operator configures
//! the one call-to-action.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::domain::{IssuerState, OnChain, PotState, ReserveState};
use styx_core::elements::hashes::Hash;
use styx_core::elements::{BlockHash, OutPoint, Script, Txid};
use styx_core::oracle::{OracleSlot, OracleTick, SignedQuote};
use styx_core::units::{BlockHeight, Obol, Price, RatioK, Sats};
use styx_explorer::render::{page, CSP};
use styx_explorer::state::ExplorerState;
use styx_watch::index::{IndexState, TrackedVault};

#[test]
fn the_dependency_graph_carries_no_signing_crate() {
    // Zero keys is a graph fact: the lib closure is core / node / watch. The check scopes
    // to [dependencies] - dev-dependencies drive the e2e and never enter the shipped
    // binary's closure.
    let manifest = include_str!("../Cargo.toml");
    let deps: String = manifest
        .split("[dev-dependencies]")
        .next()
        .unwrap()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#')) // prose may name them; dep lines may not
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in ["styx-pset", "styx-wallet", "styx-keeper", "styx-app"] {
        assert!(!deps.contains(forbidden), "{forbidden} entered the explorer's dependency closure");
    }
}

#[test]
fn the_csp_allows_no_script_at_all() {
    assert!(!CSP.contains("script-src"), "the explorer ships no script; none may load");
    assert!(CSP.contains("default-src 'none'"));
    assert!(CSP.contains("frame-ancestors 'none'"));
}

fn outpoint(n: u8) -> OutPoint {
    OutPoint::new(Txid::from_slice(&[n; 32]).unwrap(), 0)
}

/// A synthetic protocol snapshot: three singletons and three vaults straddling the bands
/// at a $100k tick (dummy signatures - nothing in the view path verifies).
fn synthetic() -> std::sync::Arc<ExplorerState> {
    let mut index = IndexState::genesis(BlockHash::from_slice(&[9u8; 32]).unwrap());
    index.height = 120;
    index.pot = Some(OnChain { state: PotState, outpoint: outpoint(1), value: Obol::new(95_000_000) });
    index.reserve =
        Some(OnChain { state: ReserveState, outpoint: outpoint(2), value: Sats::new(18_000_000) });
    index.issuer = Some(OnChain {
        state: IssuerState { last_mint_height: BlockHeight::new(100) },
        outpoint: outpoint(3),
        value: 1,
    });
    let vault = |debt: u64, coll: u64| TrackedVault {
        debt: Obol::new(debt),
        last_height: BlockHeight::new(100),
        value: Sats::new(coll),
        owner: None,
        spk: Script::new(),
    };
    // $50k debt at $100k: par is 50M sats. 160% / 125% / 90%.
    index.vaults.insert(outpoint(0x10), vault(5_000_000, 80_000_000));
    index.vaults.insert(outpoint(0x11), vault(5_000_000, 62_500_000));
    index.vaults.insert(outpoint(0x12), vault(5_000_000, 45_000_000));

    let state = ExplorerState::new(index, None);
    let quote = SignedQuote { price: Price::new(100_000), sig: [0u8; 64] };
    state.set_tick(
        OracleTick::new(
            BlockHeight::new(120),
            RatioK::from_cr_percent(100),
            [
                (OracleSlot::new(0).unwrap(), quote),
                (OracleSlot::new(1).unwrap(), quote),
                (OracleSlot::new(2).unwrap(), quote),
            ],
        )
        .unwrap(),
    );
    state
}

#[test]
fn bands_match_the_keeper_ladder() {
    let view = synthetic().view();
    let by_band: Vec<(&str, u32)> =
        view.vaults.iter().map(|v| (v.band, v.cr_percent.unwrap())).collect();
    // Sorted most endangered first by the view.
    assert_eq!(by_band, vec![("bad-debt", 90), ("partial", 125), ("healthy", 160)]);
    assert_eq!(view.protocol.as_ref().unwrap().pot_units, 95_000_000);
}

#[test]
fn the_page_renders_and_stays_internal() {
    let state = synthetic();
    let html = page(&state.view(), None);
    for expected in ["bad-debt", "partial", "healthy", "125%", "95,000,000", "no quorum"] {
        // "no quorum" must NOT appear (a tick is set); everything else must.
        if expected == "no quorum" {
            assert!(!html.contains(expected));
        } else {
            assert!(html.contains(expected), "the page lost `{expected}`");
        }
    }
    assert!(!html.contains("<script"), "the explorer ships no script");
    assert!(!html.contains("http://"), "no plaintext external URL");
    // The singleton rows link out to the public explorer so anyone can verify the tracked
    // outputs. Those click-through links are the only external references on a CTA-less page;
    // nothing loads cross-origin (the CSP forbids subresources), so the viewer's IP reaches
    // liquid.network only on a deliberate click.
    for (i, _) in html.match_indices("https://") {
        assert!(
            html[i..].starts_with("https://liquid.network/testnet/tx/"),
            "unexpected external URL on the page"
        );
    }

    // The one sanctioned external reference: the operator-configured CTA, escaped.
    let with_cta = page(&state.view(), Some("https://example.org/styx-app\"><script>"));
    assert!(with_cta.contains("https://example.org/"));
    assert!(!with_cta.contains("\"><script>"), "the CTA lands escaped");
}

#[test]
fn band_boundaries_are_the_ladder_boundaries_in_sats() {
    // $50k debt at $100k: par 50M, full-liq cap 57.5M, health gate 65M. The classification
    // must flip on the exact satoshi the keeper ladder flips on - the integer CR percent
    // floors the whole (cap, cap + one percent] range to 115 and MUST NOT drive the band.
    let mut index = IndexState::genesis(BlockHash::from_slice(&[9u8; 32]).unwrap());
    let vault = |coll: u64| TrackedVault {
        debt: Obol::new(5_000_000),
        last_height: BlockHeight::new(100),
        value: Sats::new(coll),
        owner: None,
        spk: Script::new(),
    };
    let cases: [(u8, u64, &str); 6] = [
        (0x20, 49_999_999, "bad-debt"), // strictly under par
        (0x21, 50_000_000, "full-liq"), // par exactly: the inclusive floor
        (0x22, 57_500_000, "full-liq"), // the cap, inclusive
        (0x23, 57_500_001, "partial"),  // one sat over the cap - floor CR still says 115
        (0x24, 64_999_999, "partial"),  // one sat under the gate
        (0x25, 65_000_000, "healthy"),  // the strict gate
    ];
    for (n, coll, _) in cases {
        index.vaults.insert(outpoint(n), vault(coll));
    }
    let state = ExplorerState::new(index, None);
    let quote = SignedQuote { price: Price::new(100_000), sig: [0u8; 64] };
    state.set_tick(
        OracleTick::new(
            BlockHeight::new(120),
            RatioK::from_cr_percent(100),
            [
                (OracleSlot::new(0).unwrap(), quote),
                (OracleSlot::new(1).unwrap(), quote),
                (OracleSlot::new(2).unwrap(), quote),
            ],
        )
        .unwrap(),
    );
    let view = state.view();
    for (n, coll, want) in cases {
        let v = view.vaults.iter().find(|v| v.outpoint == outpoint(n).to_string()).unwrap();
        assert_eq!((v.band, v.collateral_sats), (want, coll), "collateral {coll}");
    }
    // The reviewer's exact case, pinned: cap + 1 displays 115% yet sits in the partial band.
    let over = view.vaults.iter().find(|v| v.collateral_sats == 57_500_001).unwrap();
    assert_eq!((over.cr_percent, over.band), (Some(115), "partial"));
}

#[tokio::test]
async fn every_route_carries_the_hardening_headers() {
    use styx_explorer::serve::{router, App};
    use tower::ServiceExt;
    let app = std::sync::Arc::new(App { state: synthetic(), app_url: None });
    let r = router(app);
    for path in ["/", "/api/state", "/explorer.css"] {
        let resp = r
            .clone()
            .oneshot(axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        for (h, want) in [
            ("content-security-policy", CSP),
            ("x-frame-options", "DENY"),
            ("x-content-type-options", "nosniff"),
        ] {
            assert_eq!(
                resp.headers().get(h).and_then(|v| v.to_str().ok()),
                Some(want),
                "{path} lost {h}"
            );
        }
    }
}

#[test]
fn a_scheme_smuggling_app_url_is_a_refused_config() {
    use styx_explorer::config::{ConfigError, ExplorerConfig};
    let toml = |url: &str| {
        format!(
            r#"
styxnet = "/tmp/styxnet.toml"
rpc_url = "http://127.0.0.1:18884"
rpc_user = "styx"
rpc_password = "styx"
snapshot = "/tmp/snapshot.json"
app_url = "{url}"
"#
        )
    };
    assert!(matches!(ExplorerConfig::parse(&toml("javascript:alert(1)")), Err(ConfigError::AppUrl(_))));
    assert!(matches!(ExplorerConfig::parse(&toml("http://example.org")), Err(ConfigError::AppUrl(_))));
    assert!(ExplorerConfig::parse(&toml("https://example.org/styx-app")).is_ok());
}
