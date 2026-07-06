//! styx-wallet's library surface: the session, the ops, and the config - the binary only
//! adds argument parsing, tick assembly from the relays, and printing, so the on-node tests
//! drive the same code paths a user does.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod ops;
pub mod wallet;
