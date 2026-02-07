/// Listens for incoming peer connections
pub mod p2p_server;

/// Handles public node discovery, and connection. A daemon
pub mod auto_peer;

/// Reconnects to initial nodes if all disconnect
pub mod auto_reconnect;

/// Stores all currently pending transactions, that are waiting to be mined
pub mod mempool;

/// Stores current node state, shared between threads
pub mod node_state;

/// IBD Logic
pub mod ibd;

/// Handles full node on message logic
mod behavior;

/// Enforces longest chain rule, syncs to a peer that has a higher height
mod sync;

use flexi_logger::{Duplicate, FileSpec, Logger};
use futures::future::join_all;
use log::{error, info};
use num_bigint::BigUint;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Once},
};
use tokio::net::TcpStream;

use crate::{
    core::{
        block::Block,
        blockchain::{self, Blockchain, BlockchainError},
        transaction::{Transaction, TransactionError},
    },
    full_node::{
        behavior::FullNodePeerBehavior,
        node_state::{NodeState, SharedNodeState},
    },
    node::{
        message::{Command, Message},
        peer::{PeerError, PeerHandle, create_peer},
    },
};

pub type SharedBlockchain = Arc<Blockchain>;

static LOGGER_INIT: Once = Once::new();

/// Creates a full node (SharedBlockchain and SharedNodeState), connecting to peers, accepting blocks and transactions
pub fn create_full_node(
    node_path: &str,
    disable_stdout: bool,
) -> (SharedBlockchain, SharedNodeState) {
    let node_path = PathBuf::from(node_path);

    LOGGER_INIT.call_once(|| {
        let log_path = node_path.join("logs");
        std::fs::create_dir_all(&log_path).expect("Failed to create log directory");

        let mut logger = Logger::try_with_str("info")
            .unwrap()
            .log_to_file(FileSpec::default().directory(&log_path));

        if !disable_stdout {
            logger = logger.duplicate_to_stderr(Duplicate::Info);
        }

        logger.start().ok(); // Ignore errors if logger is already set

        info!("Logger initialized for node at {:?}", node_path);
    });

    let node_state = NodeState::new_empty();
    let node_state_expiry = node_state.clone();
    node_state
        .mempool
        .start_expiry_watchdog(move |transaction| {
            let _ = node_state_expiry
                .chain_events
                .send(node_state::ChainEvent::TransactionExpiration { transaction });
        });

    let blockchain = Blockchain::new(
        node_path
            .join("blockchain")
            .to_str()
            .expect("Failed to create node path"),
    );

    (Arc::new(blockchain), node_state)
}

/// Connect to a peer
pub async fn connect_peer(
    address: SocketAddr,
    blockchain: &SharedBlockchain,
    node_state: &SharedNodeState,
) -> Result<PeerHandle, PeerError> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| PeerError::Io(format!("IO error: {e}")))?;

    let handle = create_peer(
        stream,
        FullNodePeerBehavior::new(blockchain.clone(), node_state.clone()),
        false,
    )?;
    node_state
        .connected_peers
        .write()
        .await
        .insert(address, handle.clone());

    Ok(handle)
}

/// Forward a message to all peers
pub async fn to_peers(message: Message, node_state: &SharedNodeState) {
    let peers_snapshot: Vec<_> = node_state
        .connected_peers
        .read()
        .await
        .values()
        .cloned()
        .collect();

    // Create a list of futures for all peers
    let futures = peers_snapshot.into_iter().map(|peer| {
        let message = message.clone();
        async move {
            if let Err(err) = peer.request(message).await {
                if let Err(e) = peer.kill(err.to_string()).await {
                    error!("Failed to kill peer, error: {e}");
                }
            }
        }
    });

    // Run all futures concurrently
    join_all(futures).await;
}

/// Accept a new block to the local blockchain, and forward it to all peers
pub async fn accept_block(
    blockchain: &SharedBlockchain,
    node_state: &SharedNodeState,
    new_block: Block,
) -> Result<(), BlockchainError> {
    new_block.check_completeness()?;
    let block_hash = new_block.meta.hash.unwrap(); // Unwrap is okay, we checked that block is complete

    if node_state.last_seen_block() == block_hash {
        return Ok(()); // We already processed this block
    }
    node_state.set_last_seen_block(block_hash);

    // Wait for any running add block tasks to finish, hold a lock to prevent stacking
    let _lock = node_state.processing.lock().await;

    // Validation
    blockchain::validate_block_timestamp(&new_block)?;
    blockchain.add_block(new_block.clone(), false)?;

    // Mempool, spend transactions
    node_state
        .mempool
        .spend_transactions(
            new_block
                .transactions
                .iter()
                .map(|tx| tx.transaction_id.unwrap())
                .collect(),
        )
        .await;

    info!("New block accepted: {}", block_hash.dump_base36());

    // Broadcast new block
    let _ = node_state.chain_events.send(node_state::ChainEvent::Block {
        block: new_block.clone(),
    });

    let node_state = node_state.clone();

    // Forward to all peers (non blocking)
    tokio::spawn(async move {
        to_peers(
            Message::new(Command::NewBlock { block: new_block }),
            &node_state,
        )
        .await;
    });
    Ok(())
}

/// Accept a new block to the local blockchain, and forward it to all peers
pub async fn accept_transaction(
    blockchain: &SharedBlockchain,
    node_state: &SharedNodeState,
    new_transaction: Transaction,
) -> Result<(), BlockchainError> {
    new_transaction.check_completeness()?;
    let transaction_id = new_transaction.transaction_id.unwrap(); // Unwrap is okay, we checked that tx is complete

    if node_state
        .last_seen_transactions()
        .contains(&transaction_id)
    {
        return Ok(()); // We already processed this tx
    }
    node_state.add_last_seen_transaction(transaction_id);

    if BigUint::from_bytes_be(
        &node_state
            .get_live_transaction_difficulty(blockchain.get_transaction_difficulty())
            .await,
    ) < BigUint::from_bytes_be(&*transaction_id)
    {
        return Err(BlockchainError::LiveTransactionDifficulty);
    }

    // Validation
    blockchain::validate_transaction_timestamp(&new_transaction)?;
    blockchain.get_utxos().validate_transaction(
        &new_transaction,
        &BigUint::from_bytes_be(&blockchain.get_transaction_difficulty()),
        false,
    )?;
    if !node_state
        .mempool
        .validate_transaction(&new_transaction)
        .await
    {
        return Err(TransactionError::DoubleSpend(transaction_id.dump_base36()).into());
    }

    node_state
        .mempool
        .add_transaction(new_transaction.clone())
        .await;

    // Broadcast new transaction
    let _ = node_state
        .chain_events
        .send(node_state::ChainEvent::Transaction {
            transaction: new_transaction.clone(),
        });

    let node_state = node_state.clone();

    // Forward to all peers (non blocking)
    tokio::spawn(async move {
        to_peers(
            Message::new(Command::NewTransaction {
                transaction: new_transaction,
            }),
            &node_state,
        )
        .await;
    });
    Ok(())
}
