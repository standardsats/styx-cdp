//! The wallet's pure parts: config parsing and coin selection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use styx_core::elements::hashes::Hash;
use styx_core::elements::{OutPoint, Txid};
use styx_wallet::config::{ConfigError, WalletConfig};
use styx_wallet::wallet::Wallet;

fn config_toml(auth: &str) -> String {
    format!(
        r#"
styxnet = "/tmp/styxnet.toml"
rpc_url = "http://127.0.0.1:7040"
{auth}
snapshot = "/tmp/wallet-snapshot.json"
owner_seckey = "{}"
funding_seckey = "{}"
"#,
        "0a".repeat(32),
        "0b".repeat(32),
    )
}

#[test]
fn config_parses_and_checks_auth_and_keys() {
    let cfg = WalletConfig::parse(&config_toml("rpc_user = \"u\"\nrpc_password = \"p\"")).unwrap();
    assert!(matches!(cfg.auth(), Ok(styx_node::client::Auth::UserPass(_, _))));
    assert!(cfg.owner().is_ok());
    assert!(cfg.funding().is_ok());

    let cfg = WalletConfig::parse(&config_toml("rpc_cookie = \"/tmp/cookie\"")).unwrap();
    assert!(matches!(cfg.auth(), Ok(styx_node::client::Auth::CookieFile(_))));

    // Neither or both auth modes are refused.
    let cfg = WalletConfig::parse(&config_toml("")).unwrap();
    assert!(matches!(cfg.auth(), Err(ConfigError::Auth)));

    // A malformed key is a config error, not a panic downstream.
    let bad = config_toml("rpc_cookie = \"/tmp/cookie\"").replace(&"0a".repeat(32), "zz");
    assert!(matches!(WalletConfig::parse(&bad).unwrap().owner(), Err(ConfigError::Field { .. })));
}

fn coin(n: u8, value: u64) -> (OutPoint, u64) {
    (OutPoint::new(Txid::from_slice(&[n; 32]).unwrap(), 0), value)
}

#[test]
fn dust_change_folds_into_the_fee() {
    use styx_wallet::wallet::{split_change, DUST_FLOOR, FEE};

    // Sub-dust change would get the whole shaping tx rejected by relay policy.
    assert_eq!(split_change(0), (None, FEE));
    assert_eq!(
        split_change(DUST_FLOOR - 1),
        (None, styx_core::units::Sats::new(FEE.raw() + DUST_FLOOR - 1))
    );
    assert_eq!(split_change(DUST_FLOOR), (Some(DUST_FLOOR), FEE));
    assert_eq!(split_change(1_000_000), (Some(1_000_000), FEE));
}

#[test]
fn coin_selection_prefers_the_tightest_fit() {
    let coins = [coin(1, 50_000), coin(2, 200_000), coin(3, 120_000)];
    // The smallest sufficient coin wins, not the first.
    assert_eq!(Wallet::select_at_least(&coins, 100_000), Some(coin(3, 120_000)));
    assert_eq!(Wallet::select_at_least(&coins, 200_001), None);

    // Accumulation is largest-first and stops at sufficiency.
    let taken = Wallet::select_accumulate(&coins, 250_000).unwrap();
    assert_eq!(taken, vec![coin(2, 200_000), coin(3, 120_000)]);
    assert!(Wallet::select_accumulate(&coins, 500_000).is_none());
}

#[test]
fn address_params_follow_the_chain_name() {
    use styx_wallet::wallet::address_params;

    // Compare by the bech32 HRP: the human-facing difference these params carry (pointer
    // identity of promoted consts is not guaranteed across crates).
    assert_eq!(address_params("liquidv1").bech_hrp.to_string(), "ex");
    assert_eq!(address_params("liquidtestnet").bech_hrp.to_string(), "tex");
    assert_eq!(address_params("styxnet").bech_hrp.to_string(), "ert");
    assert_eq!(address_params("elementsregtest").bech_hrp.to_string(), "ert");
}
