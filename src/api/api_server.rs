use futures::io;
use log::{error, info, warn};
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};

use crate::{
    api::requests::{Request, Response},
    blockchain_data_provider::BlockchainDataProvider,
    core::{
        difficulty::calculate_live_transaction_difficulty, transaction::TransactionError,
        utils::slice_vec,
    },
    economics::get_block_reward,
    full_node::{SharedBlockchain, accept_block, accept_transaction, node_state::SharedNodeState},
};

pub const PAGE_SIZE: u32 = 200;

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("{0}")]
    IOError(#[from] io::Error),
}

/// Server for hosting a Snap Coin API
pub struct Server {
    port: u32,
    blockchain: SharedBlockchain,
    node_state: SharedNodeState,
}

impl Server {
    /// Create a new server, do not listen for connections yet
    pub fn new(port: u32, blockchain: SharedBlockchain, node_state: SharedNodeState) -> Self {
        Server {
            port,
            blockchain,
            node_state,
        }
    }

    /// Handle a incoming connection
    async fn connection(
        mut stream: TcpStream,
        blockchain: SharedBlockchain,
        node_state: SharedNodeState,
    ) {
        loop {
            if let Err(e) = async {
                let request = Request::decode_from_stream(&mut stream).await?;
                let response = match request {
                    Request::Height => Response::Height {
                        height: blockchain.block_store().get_height() as u64,
                    },
                    Request::Block { block_hash } => Response::Block {
                        block: blockchain.block_store().get_block_by_hash(block_hash),
                    },
                    Request::BlockHash { height } => Response::BlockHash {
                        hash: blockchain
                            .block_store()
                            .get_block_hash_by_height(height as usize),
                    },
                    Request::Transaction { transaction_id } => Response::Transaction {
                        transaction: blockchain.block_store().get_transaction(transaction_id),
                    },
                    Request::TransactionsOfAddress {
                        address,
                        page: requested_page,
                    } => {
                        let mut transactions = vec![];

                        for block in blockchain.block_store().iter_blocks() {
                            let block = block?;
                            for tx in block.transactions {
                                if tx.contains_address(address) {
                                    transactions.push(
                                        tx.transaction_id.ok_or(TransactionError::MissingId)?,
                                    );
                                }
                            }
                        }

                        let page = slice_vec(
                            &transactions,
                            (requested_page * PAGE_SIZE) as usize,
                            ((requested_page + 1) * PAGE_SIZE) as usize,
                        );
                        let next_page = if page.len() != PAGE_SIZE as usize {
                            None
                        } else {
                            Some(requested_page + 1)
                        };

                        Response::TransactionsOfAddress {
                            transactions: page.to_vec(),
                            next_page,
                        }
                    }
                    Request::AvailableUTXOs {
                        address,
                        page: requested_page,
                    } => {
                        let available = blockchain
                            .get_available_transaction_outputs(address)
                            .await?;
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
                        balance: blockchain.get_utxos().calculate_confirmed_balance(address),
                    },
                    Request::Reward => Response::Reward {
                        reward: get_block_reward(blockchain.block_store().get_height()),
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
                    Request::NewBlock { new_block } => Response::NewBlock {
                        status: accept_block(&blockchain, &node_state, new_block).await,
                    },
                    Request::NewTransaction { new_transaction } => Response::NewTransaction {
                        status: accept_transaction(&blockchain, &node_state, new_transaction).await,
                    },
                    Request::Difficulty => Response::Difficulty {
                        transaction_difficulty: blockchain.get_transaction_difficulty(),
                        block_difficulty: blockchain.get_block_difficulty(),
                    },
                    Request::BlockHeight { hash } => Response::BlockHeight {
                        height: blockchain.block_store().get_block_height_by_hash(hash),
                    },
                    Request::LiveTransactionDifficulty => Response::LiveTransactionDifficulty {
                        live_difficulty: calculate_live_transaction_difficulty(
                            &blockchain.get_transaction_difficulty(),
                            node_state.mempool.mempool_size().await,
                        ),
                    },
                    Request::SubscribeToChainEvents => {
                        let mut rx = node_state.chain_events.subscribe();
                        // Start event stream task
                        loop {
                            match rx.recv().await {
                                Ok(event) => {
                                    let response = Response::ChainEvent { event };
                                    stream.write_all(&response.encode()?).await?;
                                }
                                Err(_) => break,
                            }
                        }

                        // Stop request response task
                        return Ok(());
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

                    tokio::spawn(Self::connection(
                        stream,
                        self.blockchain.clone(),
                        self.node_state.clone(),
                    ));

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
