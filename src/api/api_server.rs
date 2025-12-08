use std::sync::Arc;

use futures::io;
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::RwLock,
};

use crate::{
    api::requests::{Request, Response},
    blockchain_data_provider::BlockchainDataProvider,
    economics::get_block_reward,
    node::node::Node,
};

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("")]
    IOError(#[from] io::Error),
}

pub struct Server {
    port: u32,
    node: Arc<RwLock<Node>>,
}

impl Server {
    pub fn new(port: u32, node: Arc<RwLock<Node>>) -> Self {
        Server { port, node }
    }

    async fn connection(mut stream: TcpStream, node: Arc<RwLock<Node>>) {
        loop {
            if let Err(e) = async {
                let request = Request::decode_from_stream(&mut stream).await?;
                let response = match request {
                    Request::Height => Response::Height {
                        height: node.read().await.blockchain.get_height() as u64,
                    },
                    Request::Block { block_hash } => Response::Block {
                        block: node.read().await.blockchain.get_block_by_hash(&block_hash),
                    },
                    Request::BlockHash { height } => Response::BlockHash {
                        hash: node
                            .read()
                            .await
                            .blockchain
                            .get_block_hash_by_height(height as usize)
                            .copied(),
                    },
                    Request::Transaction { transaction_id } => {
                        let node_guard = node.read().await;
                        let mut found = None;

                        for block_hash in node_guard.blockchain.get_all_blocks() {
                            if let Some(block) = node_guard.blockchain.get_block_by_hash(block_hash)
                            {
                                for transaction in block.transactions {
                                    if transaction.transaction_id.unwrap() == transaction_id {
                                        found = Some(transaction);
                                        break;
                                    }
                                }
                            }
                            if found.is_some() {
                                break;
                            }
                        }

                        Response::Transaction { transaction: found }
                    }
                    Request::TransactionsOfAddress { address } => {
                        let node_guard = node.read().await;
                        let mut transactions = vec![];

                        for block_hash in node_guard.blockchain.get_all_blocks() {
                            if let Some(block) =
                                node_guard.blockchain.get_block_by_hash(block_hash)
                            {
                                for transaction in block.transactions {
                                    if transaction.outputs.iter().any(|i| i.receiver == address) {
                                        transactions.push(transaction.transaction_id.unwrap());
                                        break;
                                    }
                                }
                            }
                        }
                        Response::TransactionsOfAddress { transactions }
                    }
                    Request::AvailableUTXOs { address } => Response::AvailableUTXOs {
                        available_inputs: node
                            .read()
                            .await
                            .blockchain
                            .get_available_transaction_outputs(address).await?,
                    },
                    Request::Balance { address } => Response::Balance {
                        balance: node
                            .read()
                            .await
                            .blockchain
                            .get_utxos()
                            .calculate_confirmed_balance(address),
                    },
                    Request::Reward => Response::Reward {
                        reward: get_block_reward(node.read().await.blockchain.get_height()),
                    },
                    Request::Peers => {
                        let node_guard = node.read().await;

                        let mut peers = vec![];
                        for peer in &node_guard.peers {
                            peers.push(peer.read().await.address);
                        }
                        Response::Peers { peers }
                    }
                    Request::Mempool => Response::Mempool {
                        mempool: node.read().await.mempool.get_mempool().await,
                    },
                    Request::NewBlock { new_block } => Response::NewBlock {
                        status: Node::submit_block(node.clone(), new_block).await,
                    },
                    Request::NewTransaction { new_transaction } => Response::NewTransaction {
                        status: Node::submit_transaction(node.clone(), new_transaction).await,
                    },
                    Request::Difficulty => Response::Difficulty {
                        transaction_difficulty: node
                            .read()
                            .await
                            .blockchain
                            .get_transaction_difficulty(),
                        block_difficulty: node.read().await.blockchain.get_block_difficulty(),
                    },
                    Request::BlockHeight { hash } => Response::BlockHeight {
                        height: node.read().await.blockchain.get_height_by_hash(&hash),
                    },
                };
                let response_buf = response.encode()?;

                stream.write_all(&response_buf).await?;

                Ok::<(), anyhow::Error>(())
            }
            .await
            {
                Node::log(format!("API client error: {}", e));
                break;
            }
        }
    }

    pub async fn listen(&self) -> Result<(), ApiError> {
        let listener = match TcpListener::bind(format!("127.0.0.1:{}", self.port)).await {
            Ok(l) => l,
            Err(_) => TcpListener::bind("127.0.0.1:0").await?,
        };
        Node::log(format!(
            "API Server listening on 127.0.0.1:{}",
            listener.local_addr()?.port()
        ));
        let node = self.node.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = async {
                    let (stream, _) = listener.accept().await?;

                    tokio::spawn(Server::connection(stream, node.clone()));

                    Ok::<(), ApiError>(())
                }
                .await
                {
                    Node::log(format!("API client failed to connect: {}", e))
                }
            }
        });

        Ok(())
    }
}
