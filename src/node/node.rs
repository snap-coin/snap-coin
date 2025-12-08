use futures::future::try_join_all;
use num_bigint::BigUint;
use std::io::Write;
use std::{
    fs,
    net::SocketAddr,
    pin::Pin,
    str::FromStr,
    sync::{Arc, OnceLock},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::RwLock,
    task::{JoinError, JoinHandle},
    time::sleep,
};

use crate::crypto::Hash;
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

    port: u32,
}

impl Node {
    /// Create a new blockchain (load / create) with default 12 nodes target
    /// WARNING: Only one instance of this struct can exist in one program
    pub fn new(node_path: &str, port: u32) -> Arc<RwLock<Self>> {
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
    async fn connect_peer(
        node: Arc<RwLock<Node>>,
        address: SocketAddr,
    ) -> Result<(Arc<RwLock<Peer>>, JoinHandle<Result<(), PeerError>>), NodeError> {
        let peer = Arc::new(RwLock::new(Peer::new(address)));
        let stream = TcpStream::connect(address).await?;

        let on_fail = |peer: Arc<RwLock<Peer>>, node: Arc<RwLock<Node>>| {
            Box::pin(async move {
                Peer::kill(peer.clone()).await;
                let peer_address = peer.read().await.address;

                let mut node_peers = node.write().await;

                let mut new_peers = Vec::new();
                for p in node_peers.peers.drain(..) {
                    let p_address = p.read().await.address;
                    if p_address != peer_address {
                        new_peers.push(p);
                    }
                }

                node_peers.peers = new_peers;
            }) as Pin<Box<dyn futures::Future<Output = ()> + Send + 'static>>
        };
        let handle = Peer::connect(peer.clone(), node, on_fail, stream).await;

        Ok((peer, handle))
    }

    /// Initialize this node, with a array of seed nodes which this node will use to connect to
    /// Starts all handlers
    /// WARNING: Can only be called once
    pub async fn init(
        node: Arc<RwLock<Node>>,
        seed_nodes: Vec<SocketAddr>,
    ) -> Result<JoinHandle<Result<(), NodeError>>, NodeError> {
        let mut peer_handles = Vec::new();
        let mut peers = Vec::new();

        for addr in seed_nodes {
            let (peer, handle) = Self::connect_peer(node.clone(), addr).await?;
            peers.push(peer);
            peer_handles.push(handle);
        }

        node.write().await.peers = peers;

        let server_handle: JoinHandle<Result<(), super::server::ServerError>> =
            Server.init(node.clone(), node.read().await.port).await;

        let node = node.clone();
        let auto_peer = tokio::spawn(async move {
            loop {
                // wait before next peer fetch
                sleep(Duration::from_secs(30)).await;

                // pull a snapshot of peer list and config outside the lock
                let (peers_snapshot, target_peers) = {
                    let guard = node.read().await;
                    (guard.peers.clone(), guard.target_peers)
                };

                // do we need more peers?
                if peers_snapshot.len() < target_peers {
                    // pick a known peer to ask for more peers
                    if let Some(fetch_peer) = peers_snapshot.get(0) {
                        // request peers without holding any lock
                        let response =
                            Peer::request(fetch_peer.clone(), Message::new(Command::GetPeers))
                                .await;

                        // request failed?
                        let Ok(response) = response else {
                            Node::log(format!(
                                "Could not request peers from {}",
                                fetch_peer.read().await.address,
                            ));
                            continue;
                        };

                        match response.command {
                            Command::SendPeers { peers } => {
                                for peer_str in peers {
                                    // parse address
                                    let addr = match SocketAddr::from_str(&peer_str) {
                                        Ok(a) => a,
                                        Err(_) => {
                                            Node::log(format!("Fetched peer had invalid address"));
                                            continue;
                                        }
                                    };

                                    // Check if already connected
                                    let exists = {
                                        let mut exists = false;
                                        let guard = node.read().await;
                                        for p in &guard.peers {
                                            if p.read().await.address.ip() == addr.ip()
                                                && p.read().await.address.port() == addr.port()
                                            {
                                                exists = true;
                                                break;
                                            }
                                        }
                                        exists
                                    };

                                    if exists {
                                        continue;
                                    }

                                    // Connect to fetched peer
                                    let new_peer = Node::connect_peer(node.clone(), addr).await;
                                    let Ok((peer_arc, _)) = new_peer else {
                                        continue;
                                    };

                                    // Add to our known peers
                                    {
                                        let mut guard = node.write().await;
                                        guard.peers.push(peer_arc);
                                    }

                                    Node::log(format!(
                                        "Connected to new peer (referred by {})",
                                        fetch_peer.read().await.address
                                    ));

                                    // Re-check peer count
                                    let peer_count = {
                                        let guard = node.read().await;
                                        guard.peers.len()
                                    };

                                    if peer_count >= target_peers {
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            #[allow(unused)]
            Ok::<(), NodeError>(())
        });

        let all_handle = tokio::spawn(async move {
            let auto_peer_error = match auto_peer.await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(NodeError::JoinError(e)),
            };
            if auto_peer_error.is_err() {
                return Err(auto_peer_error.err().unwrap());
            }

            // Run all peer connections concurrently
            if let Err(join_err) = try_join_all(peer_handles).await {
                return Err(NodeError::JoinError(join_err));
            }

            // Await server result
            match server_handle.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(NodeError::ServerError(e)),
                Err(e) => Err(NodeError::JoinError(e)),
            }
        });

        Ok(all_handle)
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
            "Submitting new tx {}",
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
}
