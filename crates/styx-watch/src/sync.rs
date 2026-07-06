//! The catch-up scan: walk blocks from the snapshot height to the node's tip through
//! `IndexState::apply_tx`. Every block's prev-hash is checked against the sealed tip; a
//! mismatch is a reorg, and the caller's recovery is a rescan from genesis (`IndexState::
//! genesis` + `catch_up` again) - on a single-producer private chain that is the rare case,
//! not the hot path.

use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_node::client::Node;
use styx_node::NodeError;
use styx_pset::Ctx;

use crate::index::{IndexState, Notice};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Node(#[from] NodeError),
    #[error("reorg at height {height}: the chain no longer extends the indexed tip")]
    Reorg { height: u32 },
}

/// Scan from `state.height + 1` to the node's tip. Returns every notice produced; the state
/// is sealed per block, so an RPC failure mid-scan loses nothing.
pub fn catch_up(
    node: &Node,
    ctx: &Ctx,
    owners: &[XOnlyPublicKey],
    state: &mut IndexState,
) -> Result<Vec<Notice>, SyncError> {
    let tip = node.height()?;
    let mut notices = Vec::new();
    for h in (state.height + 1)..=tip {
        let hash = node.block_hash(h)?;
        let block = node.block(hash)?;
        if block.header.prev_blockhash != state.tip {
            return Err(SyncError::Reorg { height: h });
        }
        for tx in &block.txdata {
            notices.extend(state.apply_tx(ctx, owners, h, tx));
        }
        state.seal(h, hash);
    }
    Ok(notices)
}
