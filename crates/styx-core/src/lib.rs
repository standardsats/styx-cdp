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

pub mod artifacts;
pub mod consts;
pub mod math;
pub mod units;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod golden;
