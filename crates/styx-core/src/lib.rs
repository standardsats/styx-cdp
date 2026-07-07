//! styx-core: the pure protocol layer of the STYX v1 CDP.
//!
//! Everything in this crate is deterministic and IO-free: unit newtypes and CR math, the frozen
//! covenant sources and their compilation, taproot address derivation, domain state types, and
//! the type-safe witness encoders. No node, no network, no filesystem at runtime.
//!
//! Layering (see ARCHITECTURE.md at the repo root):
//!   styx-node -> styx-pset -> styx-core
//!
//! The crate re-exports `simplicityhl` (and through it `simplicity` and `elements`) so every
//! crate in the workspace shares one pinned version of the toolchain types.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub use simplicityhl;
pub use simplicityhl::elements;
pub use simplicityhl::simplicity;

/// 256-bit words (oracle keys, CMRs, asset ids) - the SimplicityHL representation.
pub type U256 = simplicityhl::num::U256;

/// The shared secp256k1 context. Derivation and verification reuse it instead of building a
/// context per call; the builders call these paths in loops.
pub fn secp() -> &'static elements::secp256k1_zkp::Secp256k1<elements::secp256k1_zkp::All> {
    static SECP: std::sync::OnceLock<elements::secp256k1_zkp::Secp256k1<elements::secp256k1_zkp::All>> =
        std::sync::OnceLock::new();
    SECP.get_or_init(elements::secp256k1_zkp::Secp256k1::new)
}

pub mod artifacts;
pub mod consts;
pub mod domain;
pub mod encode;
pub mod leaves;
pub mod math;
pub mod net;
pub mod oracle;
pub mod params;
pub mod units;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod golden;
