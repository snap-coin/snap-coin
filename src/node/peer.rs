use bincode::error::EncodeError;
use std::pin::Pin;
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::{RwLock, oneshot},
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{
    core::{blockchain::BlockchainError, transaction::TransactionId, utxo::TransactionError},
    node::{
        message::{Command, Message, MessageError},
        node::Node,
        sync::sync_to_peer,
    },
};

#[derive(Error, Debug)]
pub enum PeerError {
    #[error("{0}")]
    MessageError(#[from] MessageError),

    #[error("Disconnected")]
    Disconnected,

    #[error("Blockchain error: {0}")]
    BlockchainError(#[from] BlockchainError),

    #[error("Transaction error: {0}")]
    TransactionError(#[from] TransactionError),

    #[error("Sync peer returned an invalid response")]
    SyncResponseInvalid,

    #[error("Could not find fork point with peer")]
    NoForkPoint,

    #[error("Block has invalid difficulty")]
    BadBlockDifficulty,

    #[error("Block has invalid block hash")]
    BadBlockHash,

    #[error("Block has no block hash attached")]
    NoBlockHash,

    #[error("Encode error: {0}")]
    EncodeError(#[from] EncodeError),
}

/// A struct representing one peer (peer connection. Can be both a client peer or a connected peer)
pub struct Peer {
    pub address: SocketAddr,

    // Outgoing messages waiting to be written to stream
    send_queue: VecDeque<Message>,

    // Pending requests waiting for a response (id -> oneshot sender)
    pending: HashMap<u16, oneshot::Sender<Message>>,

    // Shutdown flag
    shutdown: bool,

    seen_transactions: VecDeque<TransactionId>,
}

impl Peer {
    /// Create a new peer
    pub fn new(address: SocketAddr) -> Self {
        Self {
            address,
            send_queue: VecDeque::new(),
            pending: HashMap::new(),
            shutdown: false,
            seen_transactions: VecDeque::new(),
        }
    }

    /// Immediately terminates the peer connection
    pub async fn kill(peer: Arc<RwLock<Peer>>) {
        let mut p = peer.write().await;
        p.shutdown = true;
        p.send_queue.clear();
    }

    /// Main connection handler
    pub async fn connect<F>(
        peer: Arc<RwLock<Peer>>,
        node: Arc<RwLock<Node>>,
        on_fail: F,
        stream: TcpStream,
    ) -> JoinHandle<Result<(), PeerError>>
    where
        F: Fn(
                Arc<RwLock<Peer>>,
                Arc<RwLock<Node>>,
            ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>
            + Send
            + Sync
            + 'static,
    {
        let (mut read_stream, mut write_stream) = stream.into_split();

        // Spawn peer handler task
        tokio::spawn(async move {
            let peer_cloned = peer.clone();
            let node_cloned = node.clone();

            // Spawn ping / pong task
            let pinger = {
                let peer = peer.clone();
                let node = node.clone();
                Box::pin(async move {
                    loop {
                        {
                            let p = peer.read().await;
                            if p.shutdown {
                                return Ok(());
                            }
                        }
                        sleep(Duration::from_secs(5)).await; // 5 second ping interval
                        match Peer::request(
                            // Send Ping and wait for Pong
                            peer.clone(),
                            Message::new(Command::Ping {
                                height: node.read().await.blockchain.get_height(),
                            }),
                        )
                        .await?
                        .command
                        {
                            Command::Pong { .. } => {}
                            _ => {}
                        }
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), PeerError>(())
                })
            };

            // Spawn reader task
            let reader = {
                let peer = peer.clone();
                let node = node.clone();
                Box::pin(async move {
                    loop {
                        {
                            let p = peer.read().await;
                            if p.shutdown {
                                return Err(PeerError::Disconnected);
                            }
                        }
                        let msg = Message::from_stream(&mut read_stream).await?;
                        Peer::handle_incoming(peer.clone(), node.clone(), msg).await;
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), PeerError>(())
                })
            };

            // Spawn writer task
            let writer = {
                let peer = peer.clone();
                Box::pin(async move {
                    loop {
                        {
                            let p = peer.read().await;
                            if p.shutdown {
                                return Err(PeerError::Disconnected);
                            }
                        }

                        let maybe_msg = {
                            let mut p = peer.write().await;
                            p.send_queue.pop_front()
                        };

                        if let Some(msg) = maybe_msg {
                            msg.send(&mut write_stream).await?;
                        } else {
                            sleep(Duration::from_millis(10)).await;
                        }
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), PeerError>(())
                })
            };

            // Join all tasks
            let result = tokio::select! {
              r = reader => r,
              r = writer => r,
              r = pinger => r,
            };

            if let Err(e) = result {
                Node::log(format!(
                    "Disconnected peer: {}:{}. Error: {:?}",
                    peer.read().await.address.ip(),
                    peer.read().await.address.port(),
                    e
                ));
                let peer_cloned = peer_cloned.clone();
                let node_cloned = node_cloned.clone();
                tokio::spawn(async move {
                    on_fail(peer_cloned, node_cloned).await;
                });
            }
            Ok(())
        })
    }

    /// Handle incoming message
    async fn handle_incoming(peer: Arc<RwLock<Peer>>, node: Arc<RwLock<Node>>, message: Message) {
        {
            let mut p = peer.write().await;
            if let Some(tx) = p.pending.remove(&message.id) {
                let _ = tx.send(message);
                return;
            }
        }

        Peer::on_message(peer.clone(), node.clone(), message).await;
    }

    /// Handle incoming message
    async fn on_message(peer: Arc<RwLock<Peer>>, node: Arc<RwLock<Node>>, message: Message) {
        if let Err(err) = async {
            match message.command {
                Command::Connect => {
                    Peer::send(peer, message.make_response(Command::AcknowledgeConnection)).await;
                }
                Command::AcknowledgeConnection => {
                    Node::log(format!("Got unhandled AcknowledgeConnection"));
                }
                Command::Ping { height } => {
                    let local_height = node.read().await.blockchain.get_height();
                    Peer::send(
                        peer.clone(),
                        message.make_response(Command::Pong {
                            height: local_height,
                        }),
                    )
                    .await;

                    // Spawn async task that will independently sync to this node if needed
                    tokio::spawn(async move {
                        if local_height < height {
                            Node::log(format!("[SYNC] Starting sync!"));
                            {
                                let mut node = node.write().await;
                                if node.is_syncing {
                                    return;
                                }
                                node.is_syncing = true;
                            }
                            match sync_to_peer(node.clone(), Arc::clone(&peer), height).await {
                                Ok(()) => {
                                    Node::log(format!("[SYNC] Complete!"));
                                }
                                Err(e) => {
                                    Node::log(format!("[SYNC] Failed to sync! {}", e.to_string()));
                                }
                            }
                            node.write().await.is_syncing = false;
                        }
                    });
                }
                Command::Pong { .. } => {
                    Node::log(format!("Got unhandled Pong"));
                }
                Command::GetPeers => {
                    let peers: Vec<String> = {
                        let node_read = node.read().await;
                        let mut peer_addrs = Vec::new();
                        for p in &node_read.peers {
                            let p_addr = p.read().await.address.to_string();
                            peer_addrs.push(p_addr);
                        }
                        peer_addrs
                    };
                    let response = message.make_response(Command::SendPeers { peers });
                    Peer::send(peer, response).await;
                }
                Command::SendPeers { .. } => {
                    Node::log(format!("Got unhandled SendPeers"));
                }
                Command::NewBlock { ref block } => {
                    // Make sure block is not in the blockchain
                    if Some(node.read().await.last_seen_block) != block.hash {
                        Node::submit_block(node.clone(), block.clone()).await?;

                        Node::log(format!(
                            "New block accepted: {}",
                            block.hash.unwrap().dump_base36()
                        ));
                    }
                }
                Command::NewTransaction { ref transaction } => {
                    // Check if transaction was already seen
                    if peer
                        .read()
                        .await
                        .seen_transactions
                        .contains(&transaction.transaction_id.unwrap())
                    {
                        return Ok(());
                    }

                    Node::submit_transaction(node, transaction.clone()).await?;

                    Node::log(format!(
                        "New transaction accepted: {}",
                        transaction.transaction_id.unwrap().dump_base36()
                    ));
                }
                Command::GetBlock { block_hash } => {
                    Peer::send(
                        peer,
                        message.make_response(Command::GetBlockResponse {
                            block: node.read().await.blockchain.get_block_by_hash(&block_hash),
                        }),
                    )
                    .await;
                }
                Command::GetBlockResponse { .. } => {
                    Node::log(format!("Got unhandled SendBlock"));
                }
                Command::GetBlockHashes { start, end } => {
                    let mut block_hashes = Vec::new();
                    for i in start..end {
                        if let Some(block_hash) =
                            node.read().await.blockchain.get_block_hash_by_height(i)
                        {
                            block_hashes.push(*block_hash);
                        }
                    }
                    Peer::send(
                        peer,
                        message.make_response(Command::GetBlockHashesResponse { block_hashes }),
                    )
                    .await;
                }
                Command::GetBlockHashesResponse { .. } => {
                    Node::log(format!("Got unhandled SendBlockHashes"));
                }
            };
            Ok::<(), PeerError>(())
        }
        .await
        {
            Node::log(format!("Error processing incoming message: {err}"));
        }
    }

    /// Send a request and wait for the response
    pub async fn request(peer: Arc<RwLock<Peer>>, message: Message) -> Result<Message, PeerError> {
        let id = message.id;

        let (tx, rx) = oneshot::channel();

        {
            let mut p = peer.write().await;
            p.pending.insert(id, tx);
            p.send_queue.push_back(message);
        }

        match timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(_)) => Err(PeerError::Disconnected),
            Err(_) => Err(PeerError::Disconnected),
        }
    }

    /// Send a message to this peer, without expecting a response
    pub async fn send(peer: Arc<RwLock<Peer>>, message: Message) {
        let mut p = peer.write().await;
        p.send_queue.push_back(message);
    }

    /// Send this message to all peers but this one
    pub async fn send_to_peers(peer: Arc<RwLock<Peer>>, node: Arc<RwLock<Node>>, message: Message) {
        for i_peer in &node.read().await.peers {
            if !Arc::ptr_eq(&i_peer, &peer) {
                Peer::send(Arc::clone(i_peer), message.clone()).await;
            }
        }
    }
}
