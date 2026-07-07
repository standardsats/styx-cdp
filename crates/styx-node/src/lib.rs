//! styx-node: the only impure crate. elementsd RPC, protocol-state scanning with the
//! single-UTXO invariant, broadcast with conflict detection, and the regtest harness the
//! e2e suite runs on.
//!
//! The e2e tests are `#[ignore]`d and need a Simplicity-capable elementsd: run them with
//! `cargo test -p styx-node -- --ignored` inside `nix develop` (ELEMENTSD_EXE is set there).

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod ceremony;
pub mod client;
// The lifecycle driver and the regtest ceremony are test-harness code and panic on failure
// by design.
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod lifecycle;
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod regtest;
pub mod scan;

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("rpc {method}: {message}")]
    Rpc { method: String, message: String },
    #[error("unexpected rpc shape in {context}")]
    Shape { context: &'static str },
    #[error("wallet funding: {0}")]
    Funding(String),
}

#[derive(Debug, thiserror::Error)]
pub enum BroadcastError {
    /// The transaction double-spends a mempool or chain input. For the issuer singleton this
    /// is the expected contention mode: re-scan, rebuild, retry.
    #[error("input conflict: {0}")]
    Conflict(String),
    /// The node rejected the transaction (covenant or standardness). For a builder-produced
    /// transaction this is a bug: the prune tier gives the same verdict earlier.
    #[error("rejected: {0}")]
    Rejected(String),
    #[error(transparent)]
    Node(#[from] NodeError),
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// More than one UTXO at the pot's constant address (audit L-3): refuse to build until
    /// the fragments are merged.
    #[error("pot fragmented: {count} UTXOs at the pot address")]
    PotFragmented { count: usize },
    /// More than one UTXO at the reserve's constant address (audit L-3).
    #[error("reserve fragmented: {count} UTXOs at the reserve address")]
    ReserveFragmented { count: usize },
    #[error("no UTXO at the {role} address")]
    Missing { role: &'static str },
    #[error(transparent)]
    Node(#[from] NodeError),
}
