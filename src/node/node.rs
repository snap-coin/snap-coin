use num_bigint::BigUint;
use std::io::{Read, Write};
use std::{
    fs,
    net::SocketAddr,
    sync::{Arc, OnceLock},
};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::{
    sync::RwLock,
    task::{JoinError, JoinHandle},
};

use crate::crypto::Hash;
use crate::node::auto_peer::auto_peer;
use crate::node::server::ServerError;
use crate::{
    core::{
        block::Block,
        blockchain::{
            Blockchain, BlockchainError, validate_block_timestamp, validate_transaction_timestamp,
        },
        transaction::Transaction,
    },
    node::{
        mempool::MemPool,
        message::{Command, Message},
        peer::{Peer, PeerError},
        server::Server,
    },
};

/// Path at which this blockchain is being read and written from and too. This enforces a singleton rule where only one node can exist in one program at once
static NODE_PATH: OnceLock<String> = OnceLock::new();

#[derive(Error, Debug)]
pub enum NodeError {
    #[error("{0}")]
    PeerError(#[from] PeerError),

    #[error("TCP error: {0}")]
    IOError(#[from] std::io::Error),

    #[error("Join error: {0}")]
    JoinError(#[from] JoinError),

    #[error("Server error: {0}")]
    ServerError(#[from] super::server::ServerError),
}

/// Handles incoming connections and outbound peers
pub struct Node {
    pub peers: Vec<Arc<RwLock<Peer>>>,
    pub blockchain: Blockchain,
    pub mempool: MemPool,
    pub last_seen_block: Hash,

    // Synchronization flag
    pub is_syncing: bool,

    pub target_peers: usize,

    pub port: u16,
}

impl Node {
    /// Create a new blockchain (load / create) with default 12 nodes target
    /// WARNING: Only one instance of this struct can exist in one program
    pub fn new(node_path: &str, port: u16) -> Arc<RwLock<Self>> {
        NODE_PATH
            .set(String::from(node_path))
            .expect("Only one node can exist at once!");
        // Clear log file
        if !fs::exists(node_path).expect("failed to check if blockchain dir exists") {
            fs::create_dir(node_path).expect("Could not create blockchain directory");
        }
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(
                NODE_PATH
                    .get()
                    .expect("One blockchain instance must exist before logging")
                    .to_owned()
                    + "/info.log",
            )
            .expect("Could not open logging file!");
        Arc::new(RwLock::new(Node {
            peers: vec![],
            blockchain: Blockchain::new(node_path),
            mempool: MemPool::new(),
            is_syncing: false,
            target_peers: 12,
            port,
            last_seen_block: Hash::new_from_buf([0u8; 32]),
        }))
    }

    /// Connect to a specified peer
    pub async fn connect_peer(
        node: Arc<RwLock<Node>>,
        address: SocketAddr,
    ) -> Result<(Arc<RwLock<Peer>>, JoinHandle<Result<(), PeerError>>), NodeError> {
        let peer: Arc<RwLock<Peer>> = Arc::new(RwLock::new(Peer::new(address, false)));
        let stream = TcpStream::connect(address).await?;

        let handle = Peer::connect(peer.clone(), node, stream).await;

        Ok((peer, handle))
    }

    /// Initialize this node, with a array of seed nodes which this node will use to connect to
    /// Starts all handlers
    /// WARNING: Can only be called once
    pub async fn init(
        node: Arc<RwLock<Node>>,
        seed_nodes: Vec<SocketAddr>,
    ) -> Result<JoinHandle<Result<(), ServerError>>, NodeError> {
        let mut peers = Vec::new();

        for addr in seed_nodes {
            let (peer, _) = Self::connect_peer(node.clone(), addr).await?;
            peers.push(peer);
        }

        node.write().await.peers = peers;

        let server_handle: JoinHandle<Result<(), super::server::ServerError>> =
            Server.init(node.clone(), node.read().await.port).await;

        let node = node.clone();
        auto_peer(node.clone());

        Ok(server_handle)
    }

    /// Send some message to all peers
    pub async fn send_to_peers(node: Arc<RwLock<Node>>, message: Message) {
        for i_peer in &node.read().await.peers {
            Peer::send(Arc::clone(i_peer), message.clone()).await;
        }
    }

    /// Submit a new block to the network
    pub async fn submit_block(
        node: Arc<RwLock<Node>>,
        new_block: Block,
    ) -> Result<(), BlockchainError> {
        node.write().await.blockchain.add_block(new_block.clone())?;
        validate_block_timestamp(&new_block)?;

        // Remove transactions from mempool
        node.write()
            .await
            .mempool
            .spend_transactions(
                new_block
                    .transactions
                    .iter()
                    .map(|block_transaction| block_transaction.transaction_id.unwrap())
                    .collect(),
            )
            .await;

        Node::send_to_peers(
            node.clone(),
            Message::new(Command::NewBlock {
                block: new_block.clone(),
            }),
        )
        .await;
        {
            node.write().await.last_seen_block = new_block.hash.unwrap();
        }

        Node::log(format!(
            "New block accepted: {}",
            new_block.hash.unwrap().dump_base36()
        ));
        Ok(())
    }

    /// Submit a new transaction to the network to be mined
    pub async fn submit_transaction(
        node: Arc<RwLock<Node>>,
        new_transaction: Transaction,
    ) -> Result<(), BlockchainError> {
        let tx_difficulty =
            BigUint::from_bytes_be(&node.read().await.blockchain.get_transaction_difficulty());

        node.read()
            .await
            .blockchain
            .get_utxos()
            .validate_transaction(&new_transaction.clone(), &tx_difficulty)?;

        if !node
            .read()
            .await
            .mempool
            .validate_transaction(&new_transaction)
            .await
        {
            return Err(BlockchainError::DoubleSpend);
        }
        validate_transaction_timestamp(&new_transaction)?;

        node.write()
            .await
            .mempool
            .add_transaction(new_transaction.clone())
            .await;

        Node::send_to_peers(
            node.clone(),
            Message::new(Command::NewTransaction {
                transaction: new_transaction.clone(),
            }),
        )
        .await;
        Node::log(format!(
            "New transaction accepted: {}",
            new_transaction.transaction_id.unwrap().dump_base36()
        ));
        Ok(())
    }

    /// Log a message to the node log
    pub fn log(msg: String) {
        let mut log_file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(
                NODE_PATH
                    .get()
                    .expect("One blockchain instance must exist before logging")
                    .to_owned()
                    + "/info.log",
            )
            .expect("Could not open logging file!");
        writeln!(
            log_file,
            "[{}] {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            msg
        )
        .expect("Failed to write to logging file");
    }

    pub fn get_last_log() -> String {
        let mut log_file = fs::OpenOptions::new()
            .read(true)
            .open(
                NODE_PATH
                    .get()
                    .expect("One blockchain instance must exist before logging")
                    .to_owned()
                    + "/info.log",
            )
            .expect("Could not open logging file!");

        let mut contents = String::new();
        log_file
            .read_to_string(&mut contents)
            .expect("Failed to read logging file");

        // Split by newlines and get the last line
        contents.lines().last().unwrap_or("").to_string()
    }

    pub fn pop_last_line() -> Option<String> {
        let path = NODE_PATH
            .get()
            .expect("One blockchain instance must exist before logging")
            .to_owned()
            + "/info.log";

        // Read the full log
        let contents = fs::read_to_string(&path).ok()?;

        // Split into lines
        let mut lines: Vec<&str> = contents.lines().collect();

        // Pop the last line
        let last_line = lines.pop().map(|s| s.to_string());

        // Write back the remaining lines
        let new_contents = lines.join("\n");
        fs::write(&path, new_contents).ok()?;

        last_line
    }
}
