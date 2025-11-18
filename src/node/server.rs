use std::sync::Arc;
use thiserror::Error;
use tokio::{net::TcpListener, sync::RwLock, task::{self, JoinHandle}};

use crate::node::{message::Message, node::Node, peer::{Peer, Stream}};

#[derive(Error, Debug)]
pub enum ServerError {
  #[error("IO Error")]
  IoError(#[from] std::io::Error)
}

pub struct Server {
  pub peers: Vec<Peer>
}

impl Server {
  pub fn new() -> Self {
    Server {
      peers: vec![]
    }
  }

  pub async fn init(&self, node: Arc<RwLock<Node>>) -> Result<JoinHandle<()>, ServerError> {
    println!("Starting server");
    let listener = match TcpListener::bind("0.0.0.0:8998").await {
      Ok(listener) => listener,
      Err(_) => TcpListener::bind("0.0.0.0:0").await?
    };
    println!("Listening on port: {}", listener.local_addr()?.port());

    let handle = task::spawn(async move {
      loop {
        match listener.accept().await {
          Ok((stream, address)) => {
            let ip = address.ip().to_string();
            let on_peer_fail = move |node: Arc<RwLock<Node>>, peer: String| {
              async move {
                node.write().await.server.peers.retain(|p| p.address != peer);
              }
            };
            let handle_message = move |message: Message, node: Arc<RwLock<Node>>, stream: Arc<Stream>| {
              async {
                Node::handle_message(message, node, stream).await
              }
            };
            let mut peer = Peer {
              address: ip,
              stream: None
            };
            match peer.connect(Arc::new(on_peer_fail), Arc::new(handle_message), Arc::clone(&node), stream, true).await {
              Ok(_) => node.write().await.server.peers.push(peer),
              Err(e) => println!("Error while trying to create client: {e}"),
            }
          },
          Err(e) => println!("Error while trying to accept client: {e}")
        }
      }
    });


    Ok(handle)
  }
}