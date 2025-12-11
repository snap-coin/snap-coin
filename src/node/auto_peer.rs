use get_if_addrs::get_if_addrs;
use std::sync::Arc;
use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio::{sync::RwLock, task, time::sleep, time::timeout};

use crate::node::{
    message::{Command, Message},
    node::Node,
    peer::Peer,
};

fn is_my_ip(ip: IpAddr) -> bool {
    // Loopback check (127.0.0.1, ::1)
    if ip.is_loopback() {
        return true;
    }

    // Enumerate all network interfaces
    if let Ok(interfaces) = get_if_addrs() {
        for interface in interfaces {
            if interface.ip() == ip {
                return true;
            }
        }
    }

    false
}

/// Auto-peer function: periodically fetches peers and connects to new ones
pub fn auto_peer(node: Arc<RwLock<Node>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut from_peer = 0usize;

        loop {
            // Wait before next peer fetch
            sleep(Duration::from_secs(30)).await;

            // Take a snapshot of peers and target_peers outside the lock
            let (peers_snapshot, target_peers) = {
                let guard = node.read().await;
                (guard.peers.clone(), guard.target_peers)
            };

            // Skip if we already have enough peers
            if peers_snapshot.len() >= target_peers || peers_snapshot.is_empty() {
                continue;
            }

            // Pick a known peer to ask for more peers
            let fetch_peer = peers_snapshot
                .get(from_peer % peers_snapshot.len())
                .cloned();
            from_peer += 1;

            if let Some(fetch_peer) = fetch_peer {
                let response =
                    Peer::request(fetch_peer.clone(), Message::new(Command::GetPeers)).await;
                let Ok(response) = response else {
                    Node::log(format!(
                        "Could not request peers from {}",
                        fetch_peer.read().await.address,
                    ));
                    continue;
                };

                if let Command::SendPeers { peers } = response.command {
                    let mut tasks = vec![];

                    for peer_str in peers {
                        if let Ok(addr) = SocketAddr::from_str(&peer_str) {
                            let should_connect = {
                                let guard = node.read().await;

                                let peers_snapshot = {
                                    let guard = node.read().await;
                                    guard.peers.clone()
                                };
                                let peer_addresses: Vec<SocketAddr> = futures::future::join_all(
                                    peers_snapshot
                                        .iter()
                                        .map(|p| async move { p.read().await.address }),
                                )
                                .await;

                                // Now we can check synchronously
                                !peer_addresses.contains(&addr)
                                    && !(addr.port() == guard.port && is_my_ip(addr.ip()))
                            };

                            if !should_connect {
                                continue;
                            }

                            let node_clone = node.clone();
                            let task = task::spawn(async move {
                                // Connect with 2-second timeout
                                let result = timeout(
                                    Duration::from_secs(2),
                                    Node::connect_peer(node_clone.clone(), addr),
                                )
                                .await;

                                if let Ok(Ok((peer, _))) = result {
                                    // Add peer safely
                                    tokio::spawn(async move {
                                        tokio::task::yield_now().await;
                                        node_clone.write().await.peers.push(peer);
                                    });
                                    Node::log(format!("Connected to new peer {}", addr));
                                }
                            });

                            tasks.push(task);
                        }
                    }

                    // Wait for all connection tasks to finish
                    futures::future::join_all(tasks).await;
                }
            }
        }
    })
}
