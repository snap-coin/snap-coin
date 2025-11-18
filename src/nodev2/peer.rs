use std::{collections::{HashMap, VecDeque}, net::SocketAddr, sync::Arc, time::Duration};
use bincode::error::EncodeError;
use num_bigint::BigUint;
use thiserror::Error;
use tokio::{
  net::TcpStream,
  sync::{RwLock, oneshot},
  task::JoinHandle,
  time::{sleep, timeout},
};
use std::pin::Pin;

use crate::{core::{blockchain::BlockchainError, utxo::TransactionError}, nodev2::{
  message::{Command, Message, MessageError},
  node::Node, sync::sync_to_peer
}};

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
  EncodeError(#[from] EncodeError)
}

pub struct Peer {
  pub address: SocketAddr,

  // Outgoing messages waiting to be written to stream
  send_queue: VecDeque<Message>,

  // Pending requests waiting for a response (id -> oneshot sender)
  pending: HashMap<u16, oneshot::Sender<Message>>,

  // Shutdown flag
  shutdown: bool,

  // Synchronization flag
  is_syncing: bool
}

impl Peer {
  pub fn new(address: SocketAddr) -> Self {
    Self {
      address,
      send_queue: VecDeque::new(),
      pending: HashMap::new(),
      shutdown: false,
      is_syncing: false
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
    stream: TcpStream
  ) -> JoinHandle<Result<(), PeerError>>
  where
    F: Fn(Arc<RwLock<Peer>>, Arc<RwLock<Node>>) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> + Send + Sync + 'static
  {
    let (mut read_stream, mut write_stream) = stream.into_split();

    tokio::spawn(async move {
      let peer_cloned = peer.clone();
      let node_cloned = node.clone();

      let pinger = {
        let peer = peer.clone();
        let node = node.clone();
        Box::pin(async move {
          loop {
            {
              let p = peer.read().await;
              if p.shutdown { return Ok(()); }
            }
            sleep(Duration::from_secs(5)).await;
            match Peer::request(peer.clone(), Message::new(Command::Ping { height: node.read().await.blockchain.get_height() })).await?.command {
              Command::Pong { .. } => {}
              _ => {}
            }
          }
          #[allow(unreachable_code)]
          Ok::<(), PeerError>(())
        })
      };

      let reader = {
        let peer = peer.clone();
        let node = node.clone();
        Box::pin(async move {
          loop {
            {
              let p = peer.read().await;
              if p.shutdown { return Err(PeerError::Disconnected); }
            }
            let msg = Message::from_stream(&mut read_stream).await?;
            Peer::handle_incoming(peer.clone(), node.clone(), msg).await;
          }
          #[allow(unreachable_code)]
          Ok::<(), PeerError>(())
        })
      };

      let writer = {
        let peer = peer.clone();
        Box::pin(async move {
          loop {
            {
              let p = peer.read().await;
              if p.shutdown { return Err(PeerError::Disconnected); }
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

      let result = tokio::select! {
        r = reader => r,
        r = writer => r,
        r = pinger => r,
      };

      if let Err(_) = result {
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
  async fn handle_incoming(
    peer: Arc<RwLock<Peer>>,
    node: Arc<RwLock<Node>>,
    message: Message
  ) {
    {
      let mut p = peer.write().await;
      if let Some(tx) = p.pending.remove(&message.id) {
        let _ = tx.send(message);
        return;
      }
    }

    Peer::on_message(peer.clone(), node.clone(), message).await;
  }

  async fn on_message(peer: Arc<RwLock<Peer>>, node: Arc<RwLock<Node>>, message: Message) {
    if let Err(err) = async {
      match message.command {
        Command::Connect => { Peer::send(peer, message.make_response(Command::AcknowledgeConnection)).await; },
        Command::AcknowledgeConnection => { println!("Got unhandled AcknowlageConnection"); },
        Command::Ping { height } => {
          let local_height = node.read().await.blockchain.get_height();
          Peer::send(peer.clone(), message.make_response(Command::Pong { height: local_height })).await;

          if local_height < height {
            {
              if peer.read().await.is_syncing {
                return Ok(());
              }
              peer.write().await.is_syncing = true;
            }
            tokio::spawn(async move {
              match sync_to_peer(node, Arc::clone(&peer), height).await {
                Ok(()) => {
                  println!("[SYNC] Complete!");
                },
                Err(e) => {
                  println!("[SYNC] Failed to sync! {}", e.to_string());
                }
              }
              peer.write().await.is_syncing = false;
            });
          }

        },
        Command::Pong { .. } => { println!("Got unhandled Pong"); },
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
        },
        Command::SendPeers { .. } => { println!("Got unhandled SendPeers"); },
        Command::NewBlock { ref block } => {
          node.write().await.blockchain.add_block(block.clone())?;
          Peer::send_to_peers(peer, node, message).await;
        },
        Command::NewTransaction { ref transaction } => {
          let difficulty = BigUint::from_bytes_be(&node.read().await.blockchain.get_difficulty_manager().tx_difficulty);
          node.read().await.blockchain.get_utxos().validate_transaction(&transaction, &difficulty)?;
          println!("New transaction accepted: {}", transaction.transaction_id.unwrap().dump_base36());
          Peer::send_to_peers(peer, node, message).await;
        },
        Command::GetBlock { block_hash } => { Peer::send(peer, message.make_response(Command::GetBlockResponse { block: node.read().await.blockchain.get_block_by_hash(&block_hash) })).await; },
        Command::GetBlockResponse { .. } => { println!("Got unhandled Sendblock"); },
        Command::GetBlockHashes { start, end } => {
          let mut block_hashes = Vec::new();
          for i in start..end {
            if let Some(block_hash) = node.read().await.blockchain.get_block_hash_by_height(i) {
              block_hashes.push(*block_hash);
            }
          }
          Peer::send(peer, message.make_response(Command::GetBlockHashesResponse { block_hashes })).await;
        },
        Command::GetBlockHashesResponse { .. } => { println!("Got unhandled SendBlockHashes"); },
        Command::Acknowledge => { println!("Got unhandled Acknowlage"); },
      };
      Ok::<(), PeerError>(())
    }.await {
      println!("Error processing incoming message: {err}");
    }
  }

  /// Send a request and wait for the response
  pub async fn request(
    peer: Arc<RwLock<Peer>>,
    message: Message
  ) -> Result<Message, PeerError> {
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
