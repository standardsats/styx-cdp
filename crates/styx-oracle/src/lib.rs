//! styx-oracle's library surface: the daemon state, the block-publishing loop, the HTTP
//! router, and the config - everything `main` wires together, exposed so the tests drive
//! the same code paths.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod feed;
pub mod http;
pub mod service;
