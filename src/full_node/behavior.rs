use std::sync::Arc;

use log::{error, warn};

use crate::{
    crypto::merkle_tree::MerkleTreeProof, full_node::{SharedBlockchain, accept_block, accept_transaction, node_state::SharedNodeState, sync::sync_to_peer}, node::{
        message::{Command, Message},
        peer::{PeerError, PeerHandle},
        peer_behavior::{PeerBehavior, SharedPeerBehavior},
    }
};

pub struct FullNodePeerBehavior {
    blockchain: SharedBlockchain,
    node_state: SharedNodeState,
}

impl FullNodePeerBehavior {
    pub fn new(blockchain: SharedBlockchain, node_state: SharedNodeState) -> SharedPeerBehavior {
        Arc::new(Self {
            blockchain,
            node_state,
        })
    }
}

#[async_trait::async_trait]
impl PeerBehavior for FullNodePeerBehavior {
    async fn on_message(&self, message: Message, peer: &PeerHandle) -> Result<Message, PeerError> {
        let (blockchain, node_state) = (&self.blockchain, &self.node_state);

        let response = match message.command {
            Command::Connect => message.make_response(Command::AcknowledgeConnection),
            Command::AcknowledgeConnection => {
                return Err(PeerError::Unknown(
                    "Got unhandled AcknowledgeConnection".to_string(),
                ));
            }
            Command::Ping { height } => {
                let local = blockchain.block_store().get_height();
                if local < height && !*node_state.is_syncing.read().await {
                    *node_state.is_syncing.write().await = true;
                    let peer = peer.clone();
                    let blockchain = blockchain.clone();

                    // We need to sync to longer chain
                    let node_state = node_state.clone();
                    tokio::spawn(async move {
                        let res = sync_to_peer(&peer, &blockchain, height).await;
                        *node_state.is_syncing.write().await = false;
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
                return Err(PeerError::Unknown("Got unhandled Ping".to_string()));
            }
            Command::GetPeers => {
                let peers = node_state
                    .connected_peers
                    .read()
                    .await
                    .values()
                    .map(|peer| peer.address.to_string())
                    .collect();
                message.make_response(Command::SendPeers { peers })
            }
            Command::SendPeers { .. } => {
                return Err(PeerError::Unknown("Got unhandled SendPeers".to_string()));
            }
            Command::NewBlock { ref block } => {
                if !*node_state.is_syncing.read().await {
                    match accept_block(&blockchain, &node_state, block.clone()).await {
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
                match accept_transaction(&blockchain, &node_state, transaction.clone()).await {
                    Ok(()) => {}
                    Err(e) => {
                        warn!("Incoming transaction is invalid: {e}")
                    }
                }
                message.make_response(Command::NewTransactionResolved)
            }
            Command::NewTransactionResolved => {
                return Err(PeerError::Unknown(
                    "Got unhandled NewTransactionAccepted".to_string(),
                ));
            }
            Command::GetBlock { block_hash } => message.make_response(Command::GetBlockResponse {
                block: blockchain.block_store().get_block_by_hash(block_hash),
            }),
            Command::GetBlockResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetBlockResponse".to_string(),
                ));
            }
            Command::GetBlockHashes { start, end } => {
                let mut hashes = vec![];
                for height in start..end {
                    if let Some(hash) = blockchain.block_store().get_block_hash_by_height(height) {
                        hashes.push(hash);
                    }
                }
                message.make_response(Command::GetBlockHashesResponse {
                    block_hashes: hashes,
                })
            }
            Command::GetBlockHashesResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetBlockResponse".to_string(),
                ));
            }
            Command::GetTransactionMerkleProof {
                block,
                transaction_id,
            } => {
                if let Some(block) = blockchain.block_store().get_block_by_hash(block) {
                    let mut ids = vec![];
                    for tx in block.transactions {
                        ids.push(*tx.transaction_id.expect(
                        "Blockchain contains transaction without TX ID. This should NEVER happen.",
                    ));
                    }
                    message.make_response(Command::GetTransactionMerkleProofResponse {
                        proof: MerkleTreeProof::create_proof(&ids, *transaction_id),
                    })
                } else {
                    message
                        .make_response(Command::GetTransactionMerkleProofResponse { proof: None })
                }
            }
            Command::GetTransactionMerkleProofResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetTransactionMerkleProofResponse".to_string(),
                ));
            }
            Command::GetBlockMeta { block_hash } => {
                let block_metadata =
                    (|| Some(blockchain.block_store().get_block_by_hash(block_hash)?.meta))();
                message.make_response(Command::GetBlockMetadataResponse { block_metadata })
            }
            Command::GetBlockMetadataResponse { .. } => {
                return Err(PeerError::Unknown(
                    "Got unhandled GetBlockMetadataResponse".to_string(),
                ));
            }
        };

        Ok(response)
    }

    async fn get_height(&self) -> usize {
        self.blockchain.block_store().get_height()
    }

    async fn on_kill(&self, peer: &PeerHandle) {
        self.node_state
            .connected_peers
            .write()
            .await
            .remove(&peer.address);
    }
}
