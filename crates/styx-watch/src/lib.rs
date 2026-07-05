//! styx-watch: the shared base of the role daemons. This phase starts with the network
//! config (`styxnet.toml`, the one artifact every machine shares); the chain indexer and
//! the oracle quote client land in the next milestones.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
