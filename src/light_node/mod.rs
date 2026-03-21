use flexi_logger::{Duplicate, FileSpec, LogfileSelector, Logger};
use futures::future::join_all;
use log::{error, info};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Once, OnceLock},
};
use tokio::net::TcpStream;

use crate::{
    core::{
        block::Block,
        blockchain::{self, BlockchainError},
        difficulty::DifficultyState,
        transaction::{Transaction, TransactionError},
    },
    crypto::{Hash, keys::Public},
    light_node::{
        behavior::LightNodePeerBehavior,
        light_node_state::{LightNodeState, SharedLightNodeState},
        sync::{LightNodeSyncError, start_sync},
    },
    node::{
        message::{Command, Message},
        peer::{PeerError, PeerHandle, create_peer},
    },
};

pub mod api_server;
pub mod auto_peer;
mod behavior;
pub mod light_node_state;
pub mod sync;
pub mod transaction_store;
pub mod utxos;

static LOGGER_INIT: Once = Once::new();

static LOGGER_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Creates a full node (SharedBlockchain, SharedNodeState, log_path), connecting to peers, accepting blocks and transactions
pub fn create_light_node(
    node_path: &str,
    disable_stdout: bool,
) -> Result<(SharedLightNodeState, PathBuf), sled::Error> {
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

        info!("Starting logger...");

        if let Ok(logger) = logger.start()
            && let Ok(log_files) = logger.existing_log_files(&LogfileSelector::default())
        {
            LOGGER_PATH.set(log_files[0].clone()).unwrap();
            info!("Logger initialized for node at {:?}", log_files[0]);
        }
    });

    let node_state = LightNodeState::new(node_path.to_str().unwrap())?;
    Ok((node_state, LOGGER_PATH.get().unwrap().clone()))
}

/// Connect to a peer
pub async fn connect_peer(
    address: SocketAddr,
    node_state: &SharedLightNodeState,
) -> Result<PeerHandle, PeerError> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| PeerError::Io(format!("IO error: {e}")))?;

    let handle = create_peer(
        stream,
        LightNodePeerBehavior::new(node_state.clone()),
        false,
    )
    .await?;
    node_state
        .connected_peers
        .write()
        .await
        .insert(address, handle.clone());

    Ok(handle)
}

/// Forward a message to all peers
pub async fn to_peers(message: Message, node_state: &SharedLightNodeState) {
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
    node_state: &SharedLightNodeState,
    new_block: Block,
    is_historical: bool,
) -> Result<(), BlockchainError> {
    new_block.check_completeness()?;
    new_block.validate_block_hash()?;
    let block_hash = new_block.meta.hash.unwrap(); // Unwrap is okay, we checked that block is complete

    if node_state.last_seen_block() == block_hash {
        return Ok(()); // We already processed this block
    }
    node_state.set_last_seen_block(block_hash);

    // Wait for any running add block tasks to finish, hold a lock to prevent stacking
    let _lock = node_state.processing.lock().await;

    // Validation
    // Check if previous block hash is valid
    if node_state
        .last_seen_block_hashes()
        .iter()
        .next()
        .unwrap_or(&Hash::new_from_buf([0u8; 32]))
        == &new_block.meta.previous_block
    {
        return Err(BlockchainError::InvalidPreviousBlockHash);
    }

    if !is_historical {
        new_block.validate_block_hash()?;
    }

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

    node_state
        .utxos
        .scan_block(
            &new_block,
            node_state.get_height(),
            &node_state.transaction_store,
        )
        .map_err(|e| BlockchainError::UTXOs(e.to_string()))?;

    let difficulty_state = DifficultyState::new_default();
    *difficulty_state.transaction_difficulty.write().unwrap() = new_block.meta.tx_pow_difficulty;
    difficulty_state.update_difficulty(&new_block);
    *node_state.transaction_difficulty.write().await =
        difficulty_state.get_transaction_difficulty();

    node_state.increment_height();

    node_state.flush().await;
    info!("New block accepted: {}", block_hash.dump_base36());
    Ok(())
}

/// Accept a new block to the local blockchain, and forward it to all peers
pub async fn accept_transaction(
    node_state: &SharedLightNodeState,
    new_transaction: Transaction,
    trusted: bool,
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

    // Validation
    blockchain::validate_transaction_timestamp(&new_transaction)?;

    for input in &new_transaction.inputs {
        if !input
            .signature
            .unwrap()
            .validate_with_public(
                &input.output_owner,
                &new_transaction
                    .get_input_signing_buf()
                    .map_err(|e| BlockchainError::BincodeEncode(e.to_string()))?,
            )
            .map_err(|e| BlockchainError::BincodeEncode(e.to_string()))?
        {
            Err(TransactionError::InvalidSignature(
                transaction_id.dump_base36(),
            ))?;
        }
    }

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
    node_state.flush().await;

    if trusted {
        to_peers(
            Message::new(Command::NewTransaction {
                transaction: new_transaction,
            }),
            &node_state,
        )
        .await;
    }
    Ok(())
}

/// Pop last block
pub async fn pop_block(node_state: &SharedLightNodeState) -> Result<(), BlockchainError> {
    let _lock = node_state.processing.lock().await;

    node_state.decrement_height();

    node_state
        .transaction_store
        .remove_transactions_at_height(node_state.get_height());
    node_state
        .utxos
        .pop_block(node_state.get_height())
        .map_err(|e| BlockchainError::UTXOs(e.to_string()))?;
    node_state.flush().await;
    Ok(())
}

/// Starts tracking an address and re-syncs to track it
pub async fn start_tracking(
    node_state: &SharedLightNodeState,
    address: Public,
    restore_height: usize,
) -> Result<(), LightNodeSyncError> {
    if node_state.utxos.get_watched().any(|w| w == address) {
        info!("Already tracking {}", address.dump_base36());
        return Ok(());
    }
    info!("Now tracking {}", address.dump_base36());

    node_state.utxos.add_address(address).unwrap();
    node_state.set_height(restore_height);
    node_state.flush_sync();
    start_sync(node_state.clone()).await
}

/// Stops tracking an address
pub fn stop_tracking(node_state: &SharedLightNodeState, address: Public) {
    node_state
        .utxos
        .delete_address(address, &node_state.transaction_store)
        .unwrap();
    node_state.flush_sync();
    info!("Stopped tracking {}", address.dump_base36());
}
