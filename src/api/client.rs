use std::{
    net::{SocketAddr},
};

use tokio::{io::AsyncWriteExt, net::TcpStream, sync::Mutex};

use crate::{
    api::requests::{Request, RequestResponseError, Response},
    blockchain_data_provider::{BlockchainDataProvider, BlockchainDataProviderError},
    core::{
        block::Block, blockchain::BlockchainError, transaction::{Transaction, TransactionId, TransactionOutput}
    },
    crypto::{Hash, keys::Public},
};

pub struct Client {
    pub node: SocketAddr,
    stream: Mutex<TcpStream>,
}

impl Client {
    pub async fn connect(node: SocketAddr) -> Result<Self, std::io::Error> {
        let stream = TcpStream::connect(node).await?;
        stream.set_nodelay(true)?;
        Ok(Client {
            node,
            stream: Mutex::new(stream),
        })
    }

    pub async fn fetch(&self, request: Request) -> Result<Response, RequestResponseError> {
        let request_bytes = request.encode()?;

        self.stream
            .lock().await
            .write_all(&request_bytes).await
            .map_err(|_| RequestResponseError::Stream)?;

        Response::decode_from_stream(&mut *self.stream.lock().await).await
    }

    /// Submit a new block to the network
    pub async fn submit_block(&self, new_block: Block) -> Result<Result<(), BlockchainError>, BlockchainDataProviderError> {
        match self.fetch(Request::NewBlock { new_block }).await? {
            Response::NewBlock { status } => Ok(status),
            _ => Err(RequestResponseError::IncorrectResponse.into())
        }
    }

    /// submit a new transaction to the network
    pub async fn submit_transaction(&self, new_transaction: Transaction) -> Result<Result<(), BlockchainError>, BlockchainDataProviderError> {
        match self.fetch(Request::NewTransaction { new_transaction }).await? {
            Response::NewTransaction { status } => Ok(status),
            _ => Err(RequestResponseError::IncorrectResponse.into())
        }
    }

    /// Get current full mempool
    pub async fn get_mempool(&self) -> Result<Vec<Transaction>, BlockchainDataProviderError> {
        match self.fetch(Request::Mempool).await? {
            Response::Mempool { mempool } => Ok(mempool),
            _ => Err(RequestResponseError::IncorrectResponse.into())
        }
    }

    /// Get a balance of a public address
    pub async fn get_balance(&self, address: Public) -> Result<u64, BlockchainDataProviderError> {
        match self.fetch(Request::Balance { address }).await? {
            Response::Balance { balance } => Ok(balance),
            _ => Err(RequestResponseError::IncorrectResponse.into())
        }
    }

    /// Get a list of peers of the connected node
    pub async fn get_peers(&self) -> Result<Vec<SocketAddr>, BlockchainDataProviderError> {
        match self.fetch(Request::Peers).await? {
            Response::Peers { peers } => Ok(peers),
            _ => Err(RequestResponseError::IncorrectResponse.into())
        }
    }

    /// Get transaction, indexed by a transaction id
    /// Returns option
    pub async fn get_transaction(&self, transaction_id: &TransactionId) -> Result<Option<Transaction>, BlockchainDataProviderError> {
        match self.fetch(Request::Transaction { transaction_id: *transaction_id }).await? {
            Response::Transaction { transaction } => Ok(transaction),
            _ => Err(RequestResponseError::IncorrectResponse.into())
        }
    }


    /// Get all associated transactions of a public address
    /// Returns vector of transaction hashes
    pub async fn get_transactions_of_address(&self, address: &Public) -> Result<Vec<Hash>, BlockchainDataProviderError> {
        match self.fetch(Request::TransactionsOfAddress { address: *address }).await? {
            Response::TransactionsOfAddress { transactions } => Ok(transactions),
            _ => Err(RequestResponseError::IncorrectResponse.into())
        }
    }
}

#[async_trait::async_trait]
impl BlockchainDataProvider for Client {
    async fn get_height(&self) -> Result<usize, BlockchainDataProviderError> {
        match self.fetch(Request::Height).await? {
            Response::Height { height } => Ok(height as usize),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_reward(&self) -> Result<u64, BlockchainDataProviderError> {
        match self.fetch(Request::Reward).await? {
            Response::Reward { reward } => Ok(reward),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_block_by_height(
        &self,
        height: usize,
    ) -> Result<Option<Block>, BlockchainDataProviderError> {
        match self.fetch(Request::BlockHash {
            height: height as u64,
        }).await? {
            Response::BlockHash { hash } => match hash {
                Some(hash) => match self.fetch(Request::Block { block_hash: hash }).await? {
                    Response::Block { block } => Ok(block),
                    _ => Err(RequestResponseError::IncorrectResponse.into()),
                },
                None => return Ok(None),
            },
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_block_by_hash(&self, hash: &Hash) -> Result<Option<Block>, BlockchainDataProviderError> {
        match self.fetch(Request::Block { block_hash: *hash }).await? {
            Response::Block { block } => Ok(block),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_height_by_hash(
        &self,
        hash: &Hash,
    ) -> Result<Option<usize>, BlockchainDataProviderError> {
        match self.fetch(Request::BlockHeight { hash: *hash }).await? {
            Response::BlockHeight { height } => Ok(height),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_block_hash_by_height(
        &self,
        height: usize,
    ) -> Result<Option<Hash>, BlockchainDataProviderError> {
        match self.fetch(Request::BlockHash {
            height: height as u64,
        }).await? {
            Response::BlockHash { hash } => Ok(hash),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_transaction_difficulty(&self) -> Result<[u8; 32], BlockchainDataProviderError> {
        match self.fetch(Request::Difficulty).await? {
            Response::Difficulty {
                transaction_difficulty,
                block_difficulty: _,
            } => Ok(transaction_difficulty),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_block_difficulty(&self) -> Result<[u8; 32], BlockchainDataProviderError> {
        match self.fetch(Request::Difficulty).await? {
            Response::Difficulty {
                transaction_difficulty: _,
                block_difficulty,
            } => Ok(block_difficulty),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }

    async fn get_available_transaction_outputs(
        &self,
        address: Public,
    ) -> Result<Vec<(TransactionId, TransactionOutput, usize)>, BlockchainDataProviderError> {
        match self.fetch(Request::AvailableUTXOs { address }).await? {
            Response::AvailableUTXOs { available_inputs } => Ok(available_inputs),
            _ => Err(RequestResponseError::IncorrectResponse.into()),
        }
    }
}
