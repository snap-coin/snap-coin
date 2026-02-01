use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use log::error;
use thiserror::Error;
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::RwLock, task::JoinHandle, time::sleep};

use crate::{
    full_node::{SharedBlockchain, behavior::FullNodePeerBehavior, node_state::SharedNodeState},
    node::peer::create_peer,
};

pub const BAN_SCORE_THRESHOLD: u8 = 10;
pub const PUNISHMENT: u8 = 2;
pub type ClientHealthScores = RwLock<HashMap<IpAddr, u8>>;

#[derive(Error, Debug)]
pub enum P2PServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn start_p2p_server(
    port: u16,
    blockchain: SharedBlockchain,
    node_state: SharedNodeState,
) -> Result<JoinHandle<()>, P2PServerError> {
    let listener =
        TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port)).await?;

    let node_state_ban = node_state.clone();
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(60)).await;
            node_state_ban.decrement_punishments().await;
        }
    });

    Ok(tokio::spawn(async move {
        while let Ok((mut stream, address)) = listener.accept().await {
            if node_state.get_banned_ips().await.contains(&address.ip()) {
                match stream.shutdown().await {
                    Ok(()) => {
                        continue;
                    }
                    Err(e) => {
                        error!("Failed to deny incoming connection: {} - {e}", address.ip());
                    }
                }
            }
            match create_peer(
                stream,
                FullNodePeerBehavior::new(blockchain.clone(), node_state.clone()),
                true,
            ) {
                Ok(handle) => {
                    node_state
                        .connected_peers
                        .write()
                        .await
                        .insert(address, handle);
                }
                Err(e) => {
                    error!("Failed to connect to create (incoming) peer : {e}");
                }
            }
        }
    }))
}
