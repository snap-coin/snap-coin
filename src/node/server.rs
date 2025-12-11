use std::sync::Arc;

use thiserror::Error;
use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};

use crate::node::{
    node::Node,
    peer::{Peer, PeerError},
};

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("TCP error: {0}")]
    TcpError(#[from] std::io::Error),

    #[error("Peer error")]
    PeerError(#[from] PeerError),
}

/// Handles incoming peer connections
pub struct Server;

impl Server {
    // Start the server
    pub async fn init(&self, node: Arc<RwLock<Node>>, port: u16) -> JoinHandle<Result<(), ServerError>> {
        tokio::spawn(async move {
            let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
            Node::log(
                format!(
                    "Node listening on 0.0.0.0:{}",
                    port
                ),
            );
            let listener = Arc::new(listener);

            loop {
                let listener = listener.clone();
                let node = node.clone();
                if let Err(e) = async move {
                    let (stream, addr) = listener.accept().await?;
                    let peer = Arc::new(RwLock::new(Peer::new(addr, true)));

                    Peer::connect(peer.clone(), node.clone(), stream).await;
                    node.write().await.peers.push(peer);
                    Ok::<(), ServerError>(())
                }
                .await
                {
                    Node::log(format!("Peer error: {}", e.to_string()));
                }
            }

            #[allow(unreachable_code)]
            Ok(())
        })
    }
}
