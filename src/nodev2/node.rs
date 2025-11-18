use std::{net::SocketAddr, pin::Pin, sync::Arc};
use thiserror::Error;
use tokio::{
  net::TcpStream,
  sync::RwLock,
  task::{JoinError, JoinHandle},
};
use futures::future::try_join_all;

use crate::{
  core::blockchain::Blockchain, nodev2::message::Message, nodev2::{peer::{Peer, PeerError}, server::Server}
};

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

pub struct Node {
  pub peers: Vec<Arc<RwLock<Peer>>>,
  pub blockchain: Blockchain,
}

impl Node {
  pub fn new(blockchain_path: &str) -> Arc<RwLock<Self>> {
    Arc::new(RwLock::new(Node {
      peers: vec![],
      blockchain: Blockchain::new(blockchain_path),
    }))
  }

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
      }) as Pin<Box<(dyn futures::Future<Output = ()> + Send + 'static)>>
    };
    let handle = Peer::connect(peer.clone(), node, on_fail, stream).await;

    Ok((peer, handle))
  }

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

    let server_handle: JoinHandle<Result<(), super::server::ServerError>> = Server.init(node.clone()).await;

    let all_handle = tokio::spawn(async move {
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

  pub async fn send_to_peers(node: Arc<RwLock<Node>>, message: Message) {
    for i_peer in &node.read().await.peers {
      Peer::send(Arc::clone(i_peer), message.clone()).await;
    }
  }
}
