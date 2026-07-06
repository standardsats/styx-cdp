//! styx-keeper's library surface: the pure decision ladder, the keeper (purse + book +
//! step), and the config. The binary adds the relay subscription and the loop; the tests
//! drive the same step the daemon runs.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod decide;
pub mod keeper;
