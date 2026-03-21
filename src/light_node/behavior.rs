use std::sync::{Arc, atomic::Ordering};

use log::{error, warn};

use crate::{
    light_node::{
        accept_block, accept_transaction,
        light_node_state::SharedLightNodeState,
        sync::{LightNodeSyncError, start_sync},
    },
    node::{BAN_SCORE_THRESHOLD, message::{Command, ConnectionFlags, Message}, peer::{PeerError, PeerHandle}, peer_behavior::{PeerBehavior, SharedPeerBehavior}},
};

pub struct LightNodePeerBehavior {
    node_state: SharedLightNodeState,
}

impl LightNodePeerBehavior {
    pub fn new(node_state: SharedLightNodeState) -> SharedPeerBehavior {
        Arc::new(Self { node_state })
    }
}

#[async_trait::async_trait]
impl PeerBehavior for LightNodePeerBehavior {
    async fn on_message(&self, message: Message, peer: &PeerHandle) -> Result<Message, PeerError> {
        let response = match message.command {
            Command::Connect => {
                if message.version >= 4 {
                    message.make_response(Command::AcknowledgeConnectionWithFlags {
                        flags: ConnectionFlags::empty(), // We are a light node we don't serve anything
                    })
                } else {
                    message.make_response(Command::AcknowledgeConnection)
                }
            }
            Command::AcknowledgeConnection => {
                return Err(PeerError::Unknown(
                    "Got unhandled AcknowledgeConnection".to_string(),
                ));
            }
            Command::AcknowledgeConnectionWithFlags { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled AcknowledgeConnectionWithFlags".to_string(),
                ));
            }
            Command::Ping { height } => {
                let local = self.node_state.get_height();
                if local + 1 < height && !self.node_state.is_syncing.load(Ordering::SeqCst) {
                    // Need two block offset
                    self.node_state.is_syncing.store(true, Ordering::SeqCst);
                    let peer = peer.clone();

                    // We need to sync to longer chain
                    let node_state = self.node_state.clone();
                    tokio::spawn(async move {
                        node_state.mempool.clear().await; // We completely clear the mempool since syncing may invalidate, or double spend transactions
                        let res: Result<(), LightNodeSyncError> = {
                            let _lock = node_state.processing.lock().await; // Get a lock to make sure that we are not overwriting any blocks by accident
                            start_sync(node_state.clone()).await
                        };
                        node_state.is_syncing.store(false, Ordering::SeqCst);
                        match res {
                            Ok(()) => {}
                            Err(e) => {
                                if let Err(e) = peer.kill(e.to_string()).await {
                                    error!("Failed to kill peer {}, error: {e}", peer.address);
                                }
                            }
                        }
                    });
                }
                message.make_response(Command::Pong { height: local })
            }
            Command::Pong { .. } => {
                return Err(PeerError::Unknown("Got unhandled Pong".to_string()));
            }
            Command::GetPeers => {
                let peers: Vec<String> = self
                    .node_state
                    .connected_peers
                    .read()
                    .await
                    .values()
                    .filter(|peer| !peer.is_client)
                    .map(|peer| peer.address.to_string())
                    .collect();

                message.make_response(Command::SendPeers { peers })
            }
            Command::SendPeers { .. } => {
                return Err(PeerError::Unknown("Got unhandled SendPeers".to_string()));
            }
            Command::NewBlock { ref block } => {
                if !self.node_state.is_syncing.load(Ordering::SeqCst) {
                    match accept_block(&self.node_state, block.clone(), false).await {
                        Ok(()) => {}
                        Err(e) => {
                            warn!("Incoming block is invalid: {e}")
                        }
                    }
                }

                message.make_response(Command::NewBlockResolved)
            }
            Command::NewBlockResolved => {
                return Err(PeerError::Unknown(
                    "Got unhandled NewBlockAccepted".to_string(),
                ));
            }
            Command::NewTransaction { ref transaction } => {
                if !self.node_state.is_syncing.load(Ordering::SeqCst) {
                    match accept_transaction(&self.node_state, transaction.clone(), false).await {
                        Ok(()) => {}
                        Err(e) => {
                            warn!("Incoming transaction is invalid: {e}")
                        }
                    }
                }
                message.make_response(Command::NewTransactionResolved)
            }
            Command::NewTransactionResolved => {
                return Err(PeerError::Unknown(
                    "Got unhandled NewTransactionAccepted".to_string(),
                ));
            }
            Command::GetBlock { .. } => {
                message.make_response(Command::GetBlockResponse { block: None })
            }
            Command::GetBlockResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetBlockResponse".to_string(),
                ));
            }
            Command::GetBlockHashes { .. } => {
                message.make_response(Command::GetBlockHashesResponse {
                    block_hashes: vec![],
                })
            }
            Command::GetBlockHashesResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetBlockResponse".to_string(),
                ));
            }
            Command::GetTransactionMerkleProof { .. } => {
                message.make_response(Command::GetTransactionMerkleProofResponse { proof: None })
            }
            Command::GetTransactionMerkleProofResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetTransactionMerkleProofResponse".to_string(),
                ));
            }
            Command::GetBlockMetadata { .. } => {
                message.make_response(Command::GetBlockMetadataResponse {
                    block_metadata: None,
                })
            }
            Command::GetBlockMetadataResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetBlockMetadataResponse".to_string(),
                ));
            }
            Command::GetBlockMetadatas { .. } => {
                message.make_response(Command::GetBlockMetadatasResponse {
                    block_metadatas: vec![],
                })
            }
            Command::GetBlockMetadatasResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetBlockMetadatasResponse".to_string(),
                ));
            }
        };

        Ok(response)
    }

    async fn get_height(&self) -> usize {
        self.node_state.get_height()
    }

    async fn on_kill(&self, peer: &PeerHandle) {
        self.node_state
            .connected_peers
            .write()
            .await
            .remove(&peer.address);
        self.node_state.punish_ip(peer.address.ip()).await;
        if self
            .node_state
            .client_health_scores
            .read()
            .await
            .get(&peer.address.ip())
            .unwrap_or(&0)
            > &BAN_SCORE_THRESHOLD
        {
            // Extra punishment
            for _ in 0..10 {
                // 20 Min of punishment
                self.node_state.punish_ip(peer.address.ip()).await;
            }
        }
    }
}
