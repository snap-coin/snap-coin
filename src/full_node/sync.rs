use log::info;

use crate::{
    core::blockchain::BlockchainError,
    full_node::{
        SharedBlockchain,
        node_state::{self, SharedNodeState},
    },
    node::{
        message::{Command, Message},
        peer::PeerHandle,
    },
};

/// Look back max amount to find fork
pub const MAX_REORG: usize = 30;

#[derive(thiserror::Error, Debug)]
pub enum SyncError {
    #[error("Peer error: {0}")]
    PeerError(#[from] crate::node::peer::PeerError),

    #[error("Blockchain error: {0}")]
    BlockchainError(#[from] BlockchainError),

    #[error("No fork point found with peer")]
    NoForkPoint,
}

/// Synchronize local blockchain to match peer's chain using longest chain rule with fork detection
pub async fn sync_to_peer(
    peer: &PeerHandle,
    blockchain: &SharedBlockchain,
    node_state: &SharedNodeState,
    peer_height: usize,
) -> Result<(), SyncError> {
    let local_height = blockchain.block_store().get_height();
    info!(
        "[SYNC] Starting sync: local height = {}, peer height = {}",
        local_height, peer_height
    );

    // Find common ancestor (fork point), check back up to MAX_REORG blocks
    let lookback = MAX_REORG.min(local_height);
    let mut fork_height = None;

    info!(
        "[SYNC] Searching for fork point (up to {} blocks back)",
        lookback
    );
    for offset in 0..lookback {
        let height_to_check = local_height.saturating_sub(offset);
        if let Some(local_hash) = blockchain
            .block_store()
            .get_block_hash_by_height(height_to_check)
        {
            // Ask peer for the block at this height
            let msg = Message::new(Command::GetBlock {
                block_hash: local_hash,
            });
            if let Ok(response) = peer.request(msg).await {
                if let Command::GetBlockResponse {
                    block: Some(peer_block),
                } = response.command
                {
                    // If hashes match, we found the fork point
                    if peer_block.meta.hash.unwrap() == local_hash {
                        fork_height = Some(height_to_check);
                        info!("Fork point found at height {}", height_to_check);
                        break;
                    }
                }
            }
        }
    }

    let fork_height = fork_height.ok_or(SyncError::NoForkPoint)?;
    info!(
        "[SYNC] Rolling back local chain to fork height {}",
        fork_height
    );

    // Rollback local chain to fork point
    while blockchain.block_store().get_height() > fork_height {
        blockchain.pop_block()?;
    }

    info!(
        "[SYNC] Requesting block hashes from peer at height {}",
        fork_height
    );
    // Download and apply missing blocks from peer
    let mut current_height = fork_height;
    while current_height < peer_height {
        let msg = Message::new(Command::GetBlockHashes {
            start: current_height,
            end: current_height + 1,
        });
        let response = peer.request(msg).await?;
        if let Command::GetBlockHashesResponse { block_hashes } = response.command {
            if let Some(hash) = block_hashes.first() {
                let block_msg = Message::new(Command::GetBlock { block_hash: *hash });
                if let Command::GetBlockResponse { block: Some(block) } =
                    peer.request(block_msg).await?.command
                {
                    blockchain.add_block(block.clone(), false)?;
                    node_state.set_last_seen_block(block.meta.hash.unwrap());
                    // Broadcast new block
                    let _ = node_state
                        .chain_events
                        .send(node_state::ChainEvent::Block { block });
                    info!("[SYNC] Added block at height {}", current_height + 1);
                }
            }
        }
        current_height += 1;
    }

    info!(
        "[SYNC] Sync complete: local height now {}",
        blockchain.block_store().get_height()
    );
    Ok(())
}
