//! styx-explorer: the one hosted surface. Read-only protocol state over the chain indexer
//! and the public quote relay - zero keys by dependency graph (core / node / watch only;
//! the layering test keeps it that way). Server-rendered HTML with a meta refresh: the
//! page ships no script at all, and the CSP says so.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod render;
pub mod serve;
pub mod state;
