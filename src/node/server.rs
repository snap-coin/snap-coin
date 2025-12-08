use std::{pin::Pin, sync::Arc};

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
    pub async fn init(&self, node: Arc<RwLock<Node>>, port: u32) -> JoinHandle<Result<(), ServerError>> {
        tokio::spawn(async move {
            let listener = match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
                Ok(l) => l,
                Err(_) => TcpListener::bind("0.0.0.0:0").await?,
            };
            Node::log(
                format!(
                    "Node listening on 0.0.0.0:{}",
                    listener.local_addr()?.port()
                ),
            );
            let listener = Arc::new(listener);

            loop {
                let listener = listener.clone();
                let node = node.clone();
                if let Err(e) = async move {
                    let (stream, addr) = listener.accept().await?;
                    let peer = Arc::new(RwLock::new(Peer::new(addr)));

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
                        })
                            as Pin<Box<dyn futures::Future<Output = ()> + Send + 'static>>
                    };

                    Peer::connect(peer.clone(), node.clone(), on_fail, stream).await;
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
