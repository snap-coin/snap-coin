use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{api::requests::RequestResponseError, core::{block::Block, blockchain::BlockchainError, transaction::{TransactionId, TransactionOutput}}, crypto::{Hash, keys::Public}};

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum BlockchainDataProviderError {
    #[error("Failed to access data")]
    AccessError,

    #[error("Blockchain error: {0}")]
    BlockchainError(#[from] BlockchainError),

    #[error("Request / response error: {0}")]
    RequestResponseError(#[from] RequestResponseError)
}

#[async_trait::async_trait]
pub trait BlockchainDataProvider {
    /// Get current height
    async fn get_height(&self) -> Result<usize, BlockchainDataProviderError>;

    /// Get current block reward
    async fn get_reward(&self) -> Result<u64, BlockchainDataProviderError>;

    /// Get block by height
    /// Returns option
    async fn get_block_by_height(&self, height: usize) -> Result<Option<Block>, BlockchainDataProviderError>;
    
    /// Get block by hash
    /// Returns option
    async fn get_block_by_hash(&self, hash: &Hash) -> Result<Option<Block>, BlockchainDataProviderError>;

    /// Get block height by its hash
    /// Returns option
    async fn get_height_by_hash(&self, hash: &Hash) -> Result<Option<usize>, BlockchainDataProviderError>;
    
    /// Get blocks hash by its block height at which it was added
    /// Returns option
    async fn get_block_hash_by_height(&self, height: usize) -> Result<Option<Hash>, BlockchainDataProviderError>;
    
    /// Gets current transaction pow difficulty
    async fn get_transaction_difficulty(&self) -> Result<[u8; 32], BlockchainDataProviderError>;

    /// Gets current block pow difficulty
    async fn get_block_difficulty(&self) -> Result<[u8; 32], BlockchainDataProviderError>;

    /// Gets all available unspent transactions for an address
    /// Returns a list of unspent outputs (sometimes referred to as inputs as well) and their associated transaction and index in the transactions outputs
    async fn get_available_transaction_outputs(&self, address: Public) -> Result<Vec<(TransactionId, TransactionOutput, usize)>, BlockchainDataProviderError>;
}