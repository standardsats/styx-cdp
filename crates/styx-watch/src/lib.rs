//! styx-watch: the shared base of the role daemons - the network config (`styxnet.toml`,
//! the one artifact every machine shares), the chain indexer with its snapshot file and
//! catch-up scan, and the oracle quote client (verified book + Nostr transport).

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod index;
pub mod nostr;
pub mod quotes;
pub mod snapshot;
pub mod sync;
pub mod transport;
