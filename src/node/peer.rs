use bincode::error::EncodeError;
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
    core::{block::BlockError, blockchain::BlockchainError, transaction::TransactionError},
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

    #[error("Block error: {0}")]
    BlockError(#[from] BlockError)
}

pub const TIMEOUT: Duration = Duration::from_secs(15);

/// A struct representing one peer (peer connection. Can be both a client peer or a connected peer)
pub struct Peer {
    pub address: SocketAddr,

    pub is_client: bool,

    // Outgoing messages waiting to be written to stream
    send_queue: VecDeque<Message>,

    // Pending requests waiting for a response (id -> oneshot sender)
    pending: HashMap<u16, oneshot::Sender<Message>>,

    shutdown: bool,
}

impl Peer {
    /// Create a new peer
    pub fn new(address: SocketAddr, is_client: bool) -> Self {
        Self {
            address,
            is_client,
            send_queue: VecDeque::new(),
            pending: HashMap::new(),
            shutdown: false,
        }
    }

    async fn on_fail(peer: Arc<RwLock<Peer>>, node: Arc<RwLock<Node>>) {
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
    }

    /// Main connection handler
    pub async fn connect(
        peer: Arc<RwLock<Peer>>,
        node: Arc<RwLock<Node>>,
        stream: TcpStream,
    ) -> JoinHandle<Result<(), PeerError>> {
        let (mut read_stream, mut write_stream) = stream.into_split();

        // Spawn peer handler task
        tokio::spawn(async move {
            let peer_cloned = peer.clone();
            let node_cloned = node.clone();
            // Spawn ping / pong task
            let pinger = {
                let peer_outer = peer.clone();
                let node_outer = node.clone();

                Box::pin(async move {
                    loop {
                        sleep(Duration::from_secs(5)).await;
                        if peer_outer.read().await.shutdown {
                            return Err(PeerError::Disconnected);
                        }

                        let height = node_outer.read().await.blockchain.get_height();

                        let response = Peer::request(
                            peer_outer.clone(),
                            Message::new(Command::Ping { height }),
                        )
                        .await?;

                        if let Command::Pong { height } = response.command {
                            let local_height = node_outer.read().await.blockchain.get_height();

                            if local_height < height {
                                let node_for_task = node_outer.clone();
                                let peer_for_task = peer_outer.clone();

                                tokio::spawn(async move {
                                    if node_for_task.read().await.is_syncing {
                                        return;
                                    }

                                    node_for_task.write().await.is_syncing = true;

                                    let result = sync_to_peer(
                                        node_for_task.clone(),
                                        peer_for_task.clone(),
                                        height,
                                    )
                                    .await;

                                    if let Err(e) = result {
                                        Node::log(format!(
                                            "[SYNC] Failed: {}, disconnecting from {}",
                                            e,
                                            peer_for_task.read().await.address
                                        ));
                                        peer_for_task.write().await.shutdown = true;

                                        let node_for_task = node_for_task.clone();
                                        Peer::on_fail(peer_for_task, node_for_task).await;
                                    } else {
                                        Node::log("[SYNC] Completed".to_string());
                                    }

                                    node_for_task.write().await.is_syncing = false;
                                });
                            }
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
                        if peer.read().await.shutdown {
                            return Err(PeerError::Disconnected);
                        }
                        let msg = Message::from_stream(&mut read_stream).await?;
                        match timeout(
                            TIMEOUT,
                            Peer::handle_incoming(peer.clone(), node.clone(), msg),
                        )
                        .await
                        {
                            Ok(()) => {}
                            Err(..) => return Err(PeerError::Disconnected),
                        }
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
                        if peer.read().await.shutdown {
                            return Err(PeerError::Disconnected);
                        }
                        let maybe_msg = {
                            let mut p = peer.write().await;
                            p.send_queue.pop_front()
                        };

                        if let Some(msg) = maybe_msg {
                            match timeout(TIMEOUT, msg.send(&mut write_stream)).await {
                                Ok(e) => e?,
                                Err(..) => return Err(PeerError::Disconnected),
                            }
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
                    "Disconnected from peer: {}:{}. Error: {:?}",
                    peer.read().await.address.ip(),
                    peer.read().await.address.port(),
                    e
                ));

                tokio::spawn(async move {
                    Self::on_fail(peer_cloned, node_cloned).await;
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
                Command::Ping { height: _ } => {
                    Peer::send(
                        peer.clone(),
                        message.make_response(Command::Pong {
                            height: node.read().await.blockchain.get_height(),
                        }),
                    )
                    .await;
                }
                Command::Pong { .. } => {
                    Node::log(format!("Got unhandled Pong"));
                }
                Command::GetPeers => {
                    let peers: Vec<String> = {
                        let node_read = node.read().await;
                        let mut peer_addrs = Vec::new();
                        for p in &node_read.peers {
                            if p.read().await.is_client {
                                continue;
                            }
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
                    if Some(node.read().await.last_seen_block) != block.meta.hash && !node.read().await.is_syncing {
                        Node::submit_block(node.clone(), block.clone()).await?;
                    }
                }
                Command::NewTransaction { ref transaction } => {
                    // Check if transaction was already seen
                    if !node
                        .read()
                        .await
                        .mempool
                        .validate_transaction(transaction)
                        .await
                    {
                        return Ok(());
                    }

                    Node::submit_transaction(node, transaction.clone()).await?;
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
    pub async fn send_to_peers(node: Arc<RwLock<Node>>, message: Message) {
        // clone the peer list while holding the lock, then drop the lock
        let peers = {
            let guard = node.read().await;
            guard.peers.clone()
        };

        for peer in peers {
            // now safe to await
            Peer::send(peer, message.clone()).await;
        }
    }
}
