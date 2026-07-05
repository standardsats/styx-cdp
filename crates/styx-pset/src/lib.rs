//! styx-pset: pure transaction construction for the STYX v1 protocol.
//!
//! Builders are `(state + intent) -> Result<Built<Delta>, BuildError>` with no IO: they fix
//! the complete transaction body (which determines every sighash and spend environment),
//! refuse to build anything the covenants would reject, and return the predicted successor
//! state. `finalize` then prunes each covenant input against the fixed body and attaches the
//! Simplicity script witnesses - the same verdict the node gives, which is what the prune
//! test tier asserts on. Broadcast lives in styx-node; a prune rejection of a
//! builder-produced transaction is a bug.

#![deny(clippy::unwrap_used, clippy::expect_used)]

use styx_core::artifacts::Artifacts;
use styx_core::elements::BlockHash;
use styx_core::params::Params;

pub mod build;
pub mod error;
pub mod finalize;
pub mod intent;
pub mod layout;
pub mod plan;
pub mod pset;
pub mod sign;
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod testkit;

/// Everything a builder needs, bundled once per deploy.
pub struct Ctx {
    pub params: Params,
    pub artifacts: Artifacts,
    /// The chain's genesis hash: part of every spend environment and sighash.
    pub genesis: BlockHash,
}
