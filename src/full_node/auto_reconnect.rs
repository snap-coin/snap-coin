use std::net::SocketAddr;
use std::time::Duration;

use log::error;
use log::info;
use log::warn;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::full_node::SharedBlockchain;
use crate::full_node::connect_peer;
use crate::full_node::ibd::ibd_blockchain;
use crate::full_node::node_state::SharedNodeState;

pub fn start_auto_reconnect(
    node_state: SharedNodeState,
    blockchain: SharedBlockchain,
    resolved_peers: Vec<SocketAddr>,
    full_ibd: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(30)).await;
            if node_state.connected_peers.read().await.len() == 0 {
                warn!("All peers disconnected, trying to reconnect to seed peer");
                let res = connect_peer(resolved_peers[0], &blockchain, &node_state).await;
                match res {
                    Ok(_) => {
                        info!("Reconnection status: OK");
                        *node_state.is_syncing.write().await = true;
                        info!(
                            "Re-sync status: {:?}",
                            ibd_blockchain(node_state.clone(), blockchain.clone(), full_ibd).await
                        );
                        *node_state.is_syncing.write().await = false;
                    }
                    Err(e) => {
                        error!("Reconnection status: {}", e);
                    }
                }
            }
        }
    })
}
