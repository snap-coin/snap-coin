use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::{RwLock, watch};

use crate::{core::{difficulty::calculate_live_transaction_difficulty, transaction::TransactionId}, crypto::Hash, full_node::mempool::MemPool, node::peer::PeerHandle};

pub type SharedNodeState = Arc<NodeState>;

pub struct NodeState {
    pub connected_peers: RwLock<HashMap<SocketAddr, PeerHandle>>,
    pub mempool: MemPool,
    pub is_syncing: RwLock<bool>,
    last_seen_block_reader: watch::Receiver<Hash>,
    last_seen_block_writer: watch::Sender<Hash>,
    last_seen_transaction_reader: watch::Receiver<TransactionId>,
    last_seen_transaction_writer: watch::Sender<TransactionId>,
}

impl NodeState {
    pub fn new_empty() -> SharedNodeState {
        let (last_seen_block_writer, last_seen_block_reader) = watch::channel(Hash::new(b""));
        let (last_seen_transaction_writer, last_seen_transaction_reader) = watch::channel(TransactionId::new(b""));
        Arc::new(NodeState {
            connected_peers: RwLock::new(HashMap::new()),
            mempool: MemPool::new(),
            is_syncing: RwLock::new(false),
            last_seen_block_reader,
            last_seen_block_writer,
            last_seen_transaction_reader,
            last_seen_transaction_writer,
        })
    }

    /// Get the latest seen block
    pub fn last_seen_block(&self) -> Hash {
        self.last_seen_block_reader.borrow().clone()
    }

    /// Set a new last seen block
    pub fn set_last_seen_block(&self, hash: Hash) {
        let _ = self.last_seen_block_writer.send(hash);
    }

    /// Get the latest seen transaction
    pub fn last_seen_transaction(&self) -> TransactionId {
        self.last_seen_transaction_reader.borrow().clone()
    }

    /// Set a new last seen transaction
    pub fn set_last_seen_transaction(&self, tx_id: TransactionId) {
        let _ = self.last_seen_transaction_writer.send(tx_id);
    }

    pub async fn get_live_transaction_difficulty(&self, transaction_difficulty: [u8; 32]) -> [u8; 32] {
        calculate_live_transaction_difficulty(&transaction_difficulty, self.mempool.mempool_size().await)
    }
}
