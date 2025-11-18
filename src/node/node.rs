use std::sync::Arc;
use bincode::error::EncodeError;
use num_bigint::BigUint;
use thiserror::Error;
use tokio::{net::TcpStream, sync::RwLock, task::JoinHandle};

use crate::{
  core::{block::Block, blockchain::{Blockchain, BlockchainError}, transaction::Transaction, utxo::TransactionError},
  node::{
    message::{Command, Message, MessageError},
    peer::{Peer, PeerError, Stream}, server::{Server, ServerError}, sync::sync_to_peer,
  },
};

#[derive(Error, Debug)]
pub enum NodeError {
  #[error("{0}")]
  PeerError(#[from] PeerError),

  #[error("{0}")]
  ServerError(#[from] ServerError),

  #[error("{0}")]
  BlockchainError(#[from] BlockchainError),

  #[error("{0}")]
  TransactionError(#[from] TransactionError),

  #[error("{0}")]
  EncodeError(#[from] EncodeError),

  #[error("{0}")]
  MessageError(#[from] MessageError),

  // Sync errors
  #[error("Missing block")]
  MissingBlock,

  #[error("Could not find fork point")]
  NoForkPoint,

  #[error("Node response is invalid")]
  InvalidNodeResponse,

  #[error("Missing block hash")]
  NoBlockHash,

  #[error("Invalid block hash")]
  BadBlockHash,

  #[error("Invalid block difficulty")]
  BadBlockDifficulty
}

pub struct Node {
  pub blockchain: Blockchain,
  pub peers: Vec<Peer>,
  pub server: Server,
  pub is_re_syncing: bool
}

impl Node {
  pub fn new(blockchain_path: String, seed_nodes: Vec<String>) -> Arc<RwLock<Node>> {
    let blockchain = Blockchain::new(&blockchain_path);

    Arc::new(RwLock::new(Node {
      blockchain: blockchain,
      peers: seed_nodes.iter().map(|sn| Peer {
        address: sn.to_string(),
        stream: None
      }).collect(),
      server: Server::new(),
      is_re_syncing: false
    }))
  }

  pub async fn handle_message(message: Message, node: Arc<RwLock<Node>>, stream: Arc<Stream>) -> Option<Message> {
    let peer_address = stream
      .writer
      .read()
      .await
      .peer_addr()
      .map(|addr| addr.to_string())
      .unwrap_or_default();

    match message.command {
      Command::Connect => Some(Message::new(Command::AcknowlageConnection)),
      Command::AcknowlageConnection => {
        println!("Got unhandled connection acknowlagement");
        None
      },
      Command::Ping { .. } => Some(Message::new(Command::Pong { height: node.read().await.blockchain.get_height() })),
      Command::Pong { height: peer_height } => {
        if peer_height > node.read().await.blockchain.get_height() && !node.read().await.is_re_syncing {
          println!("Peer has higher height, going to sync to it");
          node.write().await.is_re_syncing = true;
          // Get last 30 blocks, if we exceed that amount a fork will be created
          if let Err(e) = sync_to_peer(Arc::clone(&node), stream, peer_height).await {
            println!("Error syncing to peer: {e}");
          }

          node.write().await.is_re_syncing = false;
        }
        None
      },
      Command::GetPeers => Some(Message::new(Command::SendPeers { peers: node.read().await.peers.iter().map(|p| p.address.to_string()).collect() })),
      Command::SendPeers { .. } => {
        println!("Got unhandled peers!");
        None
      },
      Command::NewBlock { ref block } => {
        if let Err(e) = node.write().await.blockchain.add_block(block.clone()) {
          eprintln!("Failed to add block: {e}");
          return None
        }
        match Self::forward_to_peers(Arc::clone(&node), message, Some(peer_address)).await {
          Ok(()) => {},
          Err(e) => println!("Error forwarding transaction: {e}")
        }
        println!("Added new block!");
        None
      },
      Command::NewTransaction { ref transaction } => {
        let blockchain = &node.write().await.blockchain;
        if let Err(e) = blockchain.get_utxos().validate_transaction(&transaction.clone(), &BigUint::from_bytes_be(&blockchain.get_difficulty_manager().tx_difficulty)) {
          eprintln!("Failed to add transaction: {e}");
          return None
        }
        match Self::forward_to_peers(Arc::clone(&node), message, Some(peer_address)).await {
          Ok(()) => {},
          Err(e) => println!("Error forwarding transaction: {e}")
        }
        println!("Added new transaction!");
        None
      },
      Command::GetBlock { block_hash } => Some(Message::new(Command::SendBlock { block: node.read().await.blockchain.get_block_by_hash(&block_hash) })),
      Command::SendBlock { .. } => {
        println!("Got unhandled block");
        None
      },
      Command::GetBlockHashes { start, end } => {
        let mut block_hashes = vec![];
        let blockchain = &node.read().await.blockchain;
        for h in start..end {
          block_hashes.push(blockchain.get_block_hash_by_height(h));
        }
        let block_hashes = block_hashes.iter().filter(|bh| bh.is_some()).map(|x| *x.unwrap()).collect();
        Some(Message::new(Command::SendBlockHashes { block_hashes}))
      },
      Command::SendBlockHashes { .. } => {
        println!("Got unhandled block hashes");
        None
      },
    }
  }

  pub async fn connect_peer(node: Arc<RwLock<Node>>, peer: &mut Peer) -> Result<JoinHandle<()>, PeerError> {
    // Define the async fail callback properly
    let on_peer_fail = move |node: Arc<RwLock<Node>>, peer: String| {
      async move {
        node.write().await.peers.retain(|p| p.address != peer);
      }
    };
    let handle_message = move |message, node, stream| {
      async {
        Self::handle_message(message, node, stream).await
      }
    };

    let stream = TcpStream::connect(&peer.address).await?;
    let handle = peer.connect(Arc::new(on_peer_fail), Arc::new(handle_message), node, stream, false).await?;
    Ok(handle)
  }

  pub async fn init(node: Arc<RwLock<Node>>) -> Result<Vec<JoinHandle<()>>, NodeError> {
    let peer_addrs = {
      let node_guard = node.read().await;
      node_guard.peers.iter().map(|p| p.address.clone()).collect::<Vec<_>>()
    }; // drop node guard

    let mut handles = vec![];

    // Connect to nodes
    for addr in peer_addrs {
      let peer = Peer {
        address: addr,
        stream: None,
      };
      handles.push(Self::connect_peer(Arc::clone(&node), &mut peer.clone()).await?);
    }

    println!("Connected to seed nodes ({})", handles.len());

    // Start server
    handles.push(node.read().await.server.init(Arc::clone(&node)).await?);
    println!("Started server");

    Ok(handles)
  }

  pub async fn forward_to_peers(
    node: Arc<RwLock<Node>>,
    message: Message,
    ignore_node: Option<String>,
  ) -> Result<(), NodeError> {
    let mut streams: Vec<Arc<Stream>> = {
      let node_guard = node.read().await;

      let mut v = Vec::new();

      for peer in &node_guard.peers {
        if let Some(ignore) = &ignore_node {
          if &peer.address == ignore { continue; }
        }
        if let Some(stream) = &peer.stream {
          v.push(Arc::clone(stream));
        }
      }

      for peer in &node_guard.server.peers {
        if let Some(ignore) = &ignore_node {
          if &peer.address == ignore { continue; }
        }
        if let Some(stream) = &peer.stream {
          v.push(Arc::clone(stream));
        }
      }

      v
    };
    for stream in streams.drain(..) {
      message.send(&mut *stream.writer.write().await).await?;
    }

    Ok(())
  }

  pub async fn submit_block(node: Arc<RwLock<Node>>, block: Block) -> Result<(), NodeError> {
    {
      let blockchain = &mut node.write().await.blockchain;
      blockchain.add_block(block.clone())?;
      println!("Added new block {}", block.hash.unwrap().dump_base36());
    }
    Self::forward_to_peers(node, Message::new(Command::NewBlock { block }), None).await?;
    Ok(())
  }

  pub async fn submit_tx(node: Arc<RwLock<Node>>, transaction: Transaction) -> Result<(), NodeError> {
    {
      let blockchain = &node.read().await.blockchain;
      blockchain.get_utxos().validate_transaction(&transaction, &BigUint::from_bytes_be(&blockchain.get_difficulty_manager().tx_difficulty))?;
    }
    Self::forward_to_peers(node, Message::new(Command::NewTransaction { transaction }), None).await?;
    Ok(())
  }
}