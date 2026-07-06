//! styx-app: the local application server. One wallet session (and, in keeper mode, the
//! duty loop on the same purse) behind a loopback-only HTTP API; the UI milestone serves
//! its static assets from the same origin.
//!
//! The security posture is the point of this crate: the listener refuses non-loopback
//! addresses at the config layer, every request carries a per-session token, and the
//! Host/Origin allowlist closes the DNS-rebinding / drive-by class that local wallet
//! daemons get probed with.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod api;
pub mod auth;
pub mod config;
pub mod ui;
