use std::{pin::Pin, sync::Arc};

use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};
use thiserror::Error;

use crate::nodev2::{node::Node, peer::{Peer, PeerError}};

#[derive(Error, Debug)]
pub enum ServerError {
  #[error("TCP error: {0}")]
  TcpError(#[from] std::io::Error),

  #[error("Peer error")]
  PeerError(#[from] PeerError)
}

pub struct Server;

impl Server {
  pub async fn init(&self, node: Arc<RwLock<Node>>) -> JoinHandle<Result<(), ServerError>> {
    tokio::spawn(async move {
      let listener = match TcpListener::bind("0.0.0.0:8998").await {
        Ok(l) => l,
        Err(_) => TcpListener::bind("0.0.0.0:0").await?
      };
      println!("Server listening on localhost:{}", listener.local_addr()?.port());
      let listener = Arc::new(listener);

      loop {
        let listener = listener.clone();
        let node = node.clone();
        if let Err(e) = async move {
          let (stream, addr) = listener.accept().await?;
          let peer = Arc::new(RwLock::new(Peer::new(addr)));

          let on_fail = |peer: Arc<RwLock<Peer>>, _node: Arc<RwLock<Node>>| {
            Box::pin(async move {
              Peer::kill(peer.clone()).await;
            }) as Pin<Box<(dyn futures::Future<Output = ()> + Send + 'static)>>
          };

          Peer::connect(peer, node, on_fail, stream).await;
          Ok::<(), ServerError>(())
        }.await {
          println!("Peer error: {}", e.to_string());
        }
      }

      #[allow(unreachable_code)]
      Ok(())
    })
  }
}