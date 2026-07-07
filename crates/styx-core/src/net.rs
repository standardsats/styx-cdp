//! Address encoding per chain. Scripts and spk derivation are network-independent;
//! only the human-facing address encoding (the bech32/blech32 HRP) differs.

use simplicityhl::elements::AddressParams;

/// The address encoding for a chain name, as reported by `getblockchaininfo`.chain or
/// carried in the deploy config.
pub fn address_params(chain: &str) -> &'static AddressParams {
    match chain {
        "liquidv1" => &AddressParams::LIQUID,
        "liquidtestnet" => &AddressParams::LIQUID_TESTNET,
        // Regtest and custom private chains (styxnet) use the elements defaults.
        _ => &AddressParams::ELEMENTS,
    }
}
