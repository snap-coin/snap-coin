use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use get_if_addrs::get_if_addrs;
use log::{error, info};
use rand::seq::IteratorRandom;
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{
    light_node::{connect_peer, light_node_state::SharedLightNodeState},
    node::{
        message::{Command, Message},
        peer::{PeerError, PeerHandle},
    },
};

/// Amount of peers, that the node is trying to achieve stable connections with
pub const TARGET_PEERS: usize = 24;
pub const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Daemon reload cycle time
pub const DAEMON_CYCLE: Duration = Duration::from_secs(30);

async fn get_peer_referrals(peer: &PeerHandle) -> Result<Vec<SocketAddr>, PeerError> {
    if let Command::SendPeers { peers } =
        peer.request(Message::new(Command::GetPeers)).await?.command
    {
        let mut referrals = vec![];
        for peer in peers {
            if let Ok(referral) = peer.parse() {
                referrals.push(referral);
            } else {
                return Err(PeerError::Unknown(format!(
                    "Peer address {peer} is invalid"
                )));
            }
        }
        return Ok(referrals);
    }
    Err(PeerError::Unknown(
        "GetPeers returned incorrect response".into(),
    ))
}

/// Start a Auto Peer daemon, that automatically finds peers to connect to via P2P
pub fn start_auto_peer(node_state: SharedLightNodeState) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let peer_count =
                node_state
                    .connected_peers
                    .read()
                    .await
                    .iter()
                    .fold(
                        0,
                        |acc, (_, peer)| if !peer.is_client { acc + 1 } else { acc },
                    );

            if peer_count >= TARGET_PEERS {
                sleep(DAEMON_CYCLE).await;
                continue;
            }

            // Get a random peer to poll for referrals
            let selected_peer = {
                let peers = node_state.connected_peers.read().await;
                let mut rng = rand::rng();
                peers.values().choose(&mut rng).cloned()
            };

            if let Some(peer) = selected_peer {
                let peer_address = peer.address;

                if let Err(e) = async {
                    // define the closure here so it lives inside this async block

                    let referrals = get_peer_referrals(&peer).await?;

                    for referral in referrals.iter().take(TARGET_PEERS) {
                        let node_state = node_state.clone();
                        let peer = peer.clone();
                        let _ = timeout(PEER_CONNECT_TIMEOUT, async move {
                            try_connect(&node_state, &peer, referral).await;
                        })
                        .await;
                    }

                    Ok::<(), PeerError>(())
                }
                .await
                {
                    error!("Auto peer failed {peer_address}, error: {e}");
                }
            }
            sleep(DAEMON_CYCLE).await;
        }
    })
}

pub async fn try_connect(
    node_state: &SharedLightNodeState,
    referrer: &PeerHandle,
    referral: &SocketAddr,
) {
    let is_my_ip = |ip: &IpAddr| {
        ip.is_loopback()
            || ip.is_unspecified()
            || get_if_addrs()
                .expect("Could not get Local machine IP addresses.")
                .iter()
                .any(|interface| interface.ip() == *ip)
    };

    if is_my_ip(&referral.ip()) {
        return;
    }
    if node_state
        .connected_peers
        .read()
        .await
        .contains_key(referral)
    {
        return;
    }
    // try to connect to peer, if cant, no biggie
    if let Ok(connected_peer) = connect_peer(*referral, &node_state).await {
        info!(
            "Connected to new peer: {}, referred by: {}, capability: {:?}",
            connected_peer.address, referrer.address, connected_peer.flags
        );
    }
}
