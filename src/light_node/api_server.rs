use anyhow::anyhow;
use futures::io;
use log::{error, info, warn};
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};

use crate::{
    core::{difficulty::calculate_live_transaction_difficulty, utils::slice_vec},
    economics::get_block_reward,
    light_node::{accept_transaction, light_node_state::SharedLightNodeState},
    requests::{Request, Response},
};

pub const PAGE_SIZE: u32 = 200;

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("{0}")]
    IOError(#[from] io::Error),
}

/// Server for hosting a Snap Coin API
pub struct LightNodeApiServer {
    port: u32,
    node_state: SharedLightNodeState,
}

impl LightNodeApiServer {
    /// Create a new server, do not listen for connections yet
    pub fn new(port: u32, node_state: SharedLightNodeState) -> Self {
        LightNodeApiServer { port, node_state }
    }

    /// Handle a incoming connection
    async fn connection(mut stream: TcpStream, node_state: SharedLightNodeState) {
        loop {
            if let Err(e) = async {
                let request = Request::decode_from_stream(&mut stream).await?;
                let response = match request {
                    Request::Height => Response::Height {
                        height: node_state.get_height() as u64,
                    },
                    Request::Block { .. } => {
                        return Err(anyhow!("Fetching blocks is not supported on a light node"));
                    }
                    Request::BlockHash { .. } => {
                        return Err(anyhow!(
                            "Fetching block hashes is not supported on a light node"
                        ));
                    }
                    Request::Transaction { transaction_id } => Response::Transaction {
                        transaction: node_state
                            .transaction_store
                            .get_transaction_and_info(transaction_id)
                            .map(|tx| tx.transaction),
                    },
                    Request::TransactionAndInfo { transaction_id } => {
                        Response::TransactionAndInfo {
                            transaction_and_info: node_state
                                .transaction_store
                                .get_transaction_and_info(transaction_id),
                        }
                    }
                    Request::TransactionsOfAddress {
                        address,
                        page: requested_page,
                    } => {
                        // Collect transactions that contain the address
                        let all_transactions: Vec<_> = node_state
                            .transaction_store
                            .iter_transactions()
                            .filter(|tx_info| tx_info.transaction.contains_address(address))
                            .map(|tx| tx.transaction.transaction_id.unwrap())
                            .collect();

                        let start = (requested_page * PAGE_SIZE) as usize;
                        let end = ((requested_page + 1) * PAGE_SIZE) as usize;

                        // Slice the vector safely
                        let page_slice = if start >= all_transactions.len() {
                            vec![]
                        } else if end > all_transactions.len() {
                            all_transactions[start..].to_vec()
                        } else {
                            all_transactions[start..end].to_vec()
                        };

                        // Determine if there is a next page
                        let next_page = if end < all_transactions.len() {
                            Some(requested_page + 1)
                        } else {
                            None
                        };

                        Response::TransactionsOfAddress {
                            transactions: page_slice,
                            next_page,
                        }
                    }

                    Request::AvailableUTXOs {
                        address,
                        page: requested_page,
                    } => {
                        let available = node_state.utxos.get_utxos(address)?.unwrap_or(vec![]);
                        let page = slice_vec(
                            &available,
                            (requested_page * PAGE_SIZE) as usize,
                            ((requested_page + 1) * PAGE_SIZE) as usize,
                        );
                        let next_page = if page.len() != PAGE_SIZE as usize {
                            None
                        } else {
                            Some(requested_page + 1)
                        };
                        Response::AvailableUTXOs {
                            available_inputs: page.to_vec(),
                            next_page,
                        }
                    }
                    Request::Balance { address } => Response::Balance {
                        balance: node_state.utxos.calculate_confirmed_balance(address),
                    },
                    Request::Reward => Response::Reward {
                        reward: get_block_reward(node_state.get_height()),
                    },
                    Request::Peers => {
                        let peers = node_state
                            .connected_peers
                            .read()
                            .await
                            .iter()
                            .map(|peer| *peer.0)
                            .collect();
                        Response::Peers { peers }
                    }
                    Request::Mempool {
                        page: requested_page,
                    } => {
                        let mempool = node_state.mempool.get_mempool().await;
                        let page = slice_vec(
                            &mempool,
                            (requested_page * PAGE_SIZE) as usize,
                            ((requested_page + 1) * PAGE_SIZE) as usize,
                        );
                        let next_page = if page.len() != PAGE_SIZE as usize {
                            None
                        } else {
                            Some(requested_page + 1)
                        };

                        Response::Mempool {
                            mempool: page.to_vec(),
                            next_page,
                        }
                    }
                    Request::NewBlock { .. } => {
                        return Err(anyhow!(
                            "Submitting blocks is not supported on a light node"
                        ));
                    }
                    Request::NewTransaction { new_transaction } => Response::NewTransaction {
                        status: accept_transaction(&node_state, new_transaction, true).await,
                    },
                    Request::Difficulty => Response::Difficulty {
                        transaction_difficulty: [0u8; 32],
                        block_difficulty: [0u8; 32],
                    },
                    Request::BlockHeight { .. } => {
                        return Err(anyhow!(
                            "Getting block height by hash is not supported on a light node"
                        ));
                    }
                    Request::LiveTransactionDifficulty => Response::LiveTransactionDifficulty {
                        live_difficulty: calculate_live_transaction_difficulty(
                            &*node_state.transaction_difficulty.read().await,
                            node_state.mempool.mempool_size().await,
                        ),
                    },
                    Request::SubscribeToChainEvents => {
                        return Err(anyhow!(
                            "Subscribing to chain events is not supported on a light node"
                        ));
                    }
                };
                let response_buf = response.encode()?;

                stream.write_all(&response_buf).await?;

                Ok::<(), anyhow::Error>(())
            }
            .await
            {
                warn!("API client error: {}", e);
                break;
            }
        }
    }

    /// Start listening for clients
    pub async fn listen(self) -> Result<(), ApiError> {
        let listener = match TcpListener::bind(format!("0.0.0.0:{}", self.port)).await {
            Ok(l) => l,
            Err(_) => TcpListener::bind("0.0.0.0:0").await?,
        };
        info!(
            "API Server listening on 0.0.0.0:{}",
            listener.local_addr()?.port()
        );

        tokio::spawn(async move {
            loop {
                if let Err(e) = async {
                    let (stream, _) = listener.accept().await?;

                    tokio::spawn(Self::connection(stream, self.node_state.clone()));

                    Ok::<(), ApiError>(())
                }
                .await
                {
                    error!("API client failed to connect: {e}")
                }
            }
        });

        Ok(())
    }
}
