use num_bigint::BigUint;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{
    crypto::Hash, node::{
        message::{Command, Message},
        node::Node,
        peer::{Peer, PeerError},
    }
};

/// Synchronize to some other peer
pub async fn sync_to_peer(
    node: Arc<RwLock<Node>>,
    peer: Arc<RwLock<Peer>>,
    peer_height: usize,
) -> Result<(), PeerError> {
    // 1. Request block hashes from the peer (No locks on Node held)
    let start = peer_height.saturating_sub(30);
    let block_hashes = {
        Node::log(format!("[SYNC] Fetching block hashes from peer"));
        let response: Message = Peer::request(
            Arc::clone(&peer),
            Message::new(Command::GetBlockHashes {
                start,
                end: peer_height,
            }),
        ).await?;
        match response.command {
            Command::GetBlockHashesResponse { block_hashes } => block_hashes,
            _ => return Err(PeerError::SyncResponseInvalid),
        }
    };
    Node::log(format!("[Sync] Fetched block hashes from peer"));

    // Find fork point: Take a brief READ LOCK on Node.
    let (fork_index, fork_height) = {
        let node_read = node.read().await; // READ LOCK acquired

        // Iterate from the peer's newest block backwards
        let mut found: Option<(usize, usize)> = None;
        for (i, h) in block_hashes.iter().enumerate().rev() {
            if let Some(height) = node_read.blockchain.get_height_by_hash(h) {
                found = Some((i, height));
                break;
            }
        }
        // READ LOCK dropped upon exiting the block
        found.ok_or(PeerError::NoForkPoint)?
    };

    Node::log(
        format!(
            "[SYNC] Replacing chain to longer chain from height: {} (fork index {})",
            fork_height, fork_index
        ),
    );

    // New hashes are those AFTER the fork index (fork_index + 1)
    let new_hashes = if fork_index + 1 <= block_hashes.len() {
        &block_hashes[fork_index + 1..]
    } else {
        &vec![] as &[Hash]
    };

    if new_hashes.is_empty() {
        Node::log(format!("[SYNC] Already synced!"));
        return Ok(());
    }

    // Fetch and validate new blocks (No locks on Node held during I/O or heavy computation)
    let mut new_blocks = Vec::with_capacity(new_hashes.len());
    for bh in new_hashes.iter() {
        // Request block from peer (I/O operation)
        let block = {
            match Peer::request(
                Arc::clone(&peer),
                Message::new(Command::GetBlock { block_hash: *bh }),
            )
            .await?
            .command
            {
                Command::GetBlockResponse { block } => {
                    block.ok_or(PeerError::SyncResponseInvalid)?
                }
                _ => return Err(PeerError::SyncResponseInvalid),
            }
        };

        // Basic validation
        // TODO: Make more robust. Possibly split into validate block and add block functions?
        let block_hash = block.hash.ok_or(PeerError::NoBlockHash)?;

        // Verify PoW difficulty
        let hash_val = BigUint::from_bytes_be(&*block_hash);
        let target = BigUint::from_bytes_be(&block.block_pow_difficulty);
        if hash_val > target {
            return Err(PeerError::BadBlockDifficulty);
        }

        // Recompute and verify block hash
        let recomputed = Hash::new(&block.get_hashing_buf()?);
        if block_hash != recomputed {
            return Err(PeerError::BadBlockHash);
        }

        new_blocks.push(block);
    }

    // Apply blockchain changes in a single WRITE LOCK (minimizing lock holding time)
    {
        let mut node_guard = node.write().await; // WRITE LOCK acquired

        let expected_previous_hash = node_guard
            .blockchain
            .get_block_hash_by_height(fork_height)
            .ok_or(PeerError::NoForkPoint)?;

        // Anti-race condition check: Verify the fork point is still valid
        if expected_previous_hash
            != node_guard
                .blockchain
                .get_block_hash_by_height(fork_height)
                .ok_or(PeerError::NoBlockHash)?
        {
            Node::log(
                format!("[SYNC] Fork point invalidated by parallel chain update. Aborting sync."),
            );
            // TODO: Returning a specific error like ForkPointChanged would be better if defined
            return Err(PeerError::SyncResponseInvalid);
        }

        // Remove blocks above fork_height
        let mut popped_blocks = 0;
        while node_guard.blockchain.get_height() > fork_height + 1 {
            node_guard.blockchain.pop_block()?;
            popped_blocks += 1;
        }

        Node::log(
            format!(
                "[SYNC] Backed local chain to height: {}, Popped {} blocks",
                node_guard.blockchain.get_height(),
                popped_blocks
            ),
        );

        // Append downloaded blocks in order
        for block in new_blocks {
            Node::log(format!("[SYNC] Adding block, current height: {}, Block hash: {}", node_guard.blockchain.get_height(), match block.hash {
                Some(hash) => hash.dump_base36(),
                None => "<missing hash>".to_owned()
            }));
            node_guard.blockchain.add_block(block)?;
        }
    }

    Node::log(
        format!(
            "Longest chain updated, local chain back up to {}",
            node.read().await.blockchain.get_height()
        ),
    );
    Ok(())
}
