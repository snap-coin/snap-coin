use std::sync::Arc;
use num_bigint::BigUint;
use tokio::sync::RwLock;

use crate::{
  core::crypto::Hash,
  node::{
    message::{Command, Message},
    node::{Node, NodeError},
    peer::Stream,
  },
};

pub async fn sync_to_peer(
  node: Arc<RwLock<Node>>,
  stream: Arc<Stream>,
  peer_height: usize,
) -> Result<(), NodeError> {
  // Request last 30 block hashes from peer (safe: compute start locally)
  let start = peer_height.saturating_sub(30);
  {
    // scope writer lock tightly
    let mut writer = stream.writer.write().await;
    Message::new(Command::GetBlockHashes { start, end: peer_height })
      .send(&mut *writer)
      .await?;
  }

  // Read peer's response
  let block_hashes = {
    let mut reader = stream.reader.write().await; // keep lock scope minimal
    match Message::from_stream(&mut *reader).await?.command {
      Command::SendBlockHashes { block_hashes } => block_hashes,
      _ => return Err(NodeError::InvalidNodeResponse),
    }
  };

  // Find fork point: the highest (latest) hash in peer's list that we already have
  let (fork_index, fork_height) = {
    let node_read = node.read().await;
    // iterate from the end (peer's newest) backwards
    let mut found: Option<(usize, usize)> = None;
    for (i, h) in block_hashes.iter().enumerate().rev() {
      if let Some(height) = node_read.blockchain.get_height_by_hash(h) {
        found = Some((i, height));
        break;
      }
    }
    found.ok_or(NodeError::NoForkPoint)?
  };

  println!("Replacing chain to longer chain from height: {} (fork index {})", fork_height, fork_index);

  // New hashes are those after the fork index in the peer's list
  let new_hashes = if fork_index + 1 <= block_hashes.len() {
    &block_hashes[fork_index..]
  } else {
    &[] as &[Hash]
  };

  // If there's nothing new, we're already synced
  if new_hashes.is_empty() {
    println!("Already synced!");
    return Ok(());
  }

  // Fetch and validate new blocks in order
  let mut new_blocks = Vec::with_capacity(new_hashes.len());
  for bh in new_hashes.iter() {
    // Request block from peer (lock writer briefly)
    {
      let mut writer = stream.writer.write().await;
      Message::new(Command::GetBlock { block_hash: *bh })
        .send(&mut *writer)
        .await?;
    }

    // Receive block (lock reader briefly)
    let block = {
      let mut reader = stream.reader.write().await;
      match Message::from_stream(&mut *reader).await?.command {
        Command::SendBlock { block } => block.ok_or(NodeError::MissingBlock)?,
        _ => return Err(NodeError::InvalidNodeResponse),
      }
    };

    // Basic validation
    let block_hash = block.hash.ok_or(NodeError::NoBlockHash)?;

    let hash_val = BigUint::from_bytes_be(&*block_hash);
    let target = BigUint::from_bytes_be(&block.block_pow_difficulty);
    if hash_val >= target {
      return Err(NodeError::BadBlockDifficulty);
    }

    // Recompute and verify block hash
    let recomputed = Hash::new(&block.get_hashing_buf()?);
    if block_hash != recomputed {
      return Err(NodeError::BadBlockHash);
    }

    new_blocks.push(block);
  }

  // Apply blockchain changes in one write lock (atomic with respect to other node changes)
  {
    let mut node_guard = node.write().await;

    // Remove blocks above fork_height
    while node_guard.blockchain.get_height() > fork_height {
      node_guard.blockchain.pop_block()?;
    }

    // Append downloaded blocks in order
    for block in new_blocks {
      node_guard.blockchain.add_block(block)?;
    }
  }

  println!("Longest chain updated, local chain back up to {}", node.read().await.blockchain.get_height());
  Ok(())
}