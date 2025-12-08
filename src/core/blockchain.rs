use bincode::{Decode, Encode, config};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::Path;
use thiserror::Error;

use crate::blockchain_data_provider::BlockchainDataProvider;
use crate::core::block::Block;
use crate::core::difficulty::{DifficultyManager, calculate_block_difficulty};
use crate::core::transaction::{Transaction, TransactionId};
use crate::core::utxo::{TransactionError, UTXODiff, UTXOs};
use crate::crypto::Hash;
use crate::economics::{
    DEV_WALLET, EXPIRATION_TIME, GENESIS_PREVIOUS_BLOCK_HASH, calculate_dev_fee, get_block_reward,
};

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum BlockchainError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("Bincode decode error: {0}")]
    BincodeDecode(String),

    #[error("Bincode encode error: {0}")]
    BincodeEncode(String),

    #[error("Block hash mismatch. Expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("Block timestamp is in the future: {0}")]
    FutureTimestamp(u64),

    #[error("Invalid block / transaction difficulty")]
    InvalidDifficulty,

    #[error("Block does not have a hash attached")]
    MissingHash,

    #[error("Transaction is invalid: {0}")]
    InvalidTransaction(String),

    #[error("Double spend detected in transaction input")]
    DoubleSpend,

    #[error("Too many reward transaction in block")]
    RewardOverspend,

    #[error("Reward transaction is invalid")]
    InvalidRewardTransaction,

    #[error("Reward transaction's outputs do not sum up to block reward amount")]
    InvalidRewardTransactionAmount,

    #[error("Reward transaction's id is missing")]
    RewardTransactionIdMissing,

    #[error("Reward transaction's outputs do not include a dev fee transaction")]
    NoDevFee,

    #[error("No blocks to to pop")]
    NoBlocksToPop,

    #[error("Block or transaction timestamp is invalid or expired")]
    InvalidTimestamp,

    #[error("Previous block hash is invalid")]
    InvalidPreviousBlockHash,
}

impl From<TransactionError> for BlockchainError {
    fn from(err: TransactionError) -> Self {
        BlockchainError::InvalidTransaction(err.to_string())
    }
}

/// BlockchainData is an object used for storing and loading current blockchain state.
#[derive(Encode, Decode, Debug, Clone)]
struct BlockchainData {
    difficulty_manager: DifficultyManager,
    height: usize,
    block_lookup: HashMap<Hash, usize>,
    utxos: UTXOs,
}

/// The blockchain, handles everything. Core to this crypto coin.
#[derive(Debug)]
pub struct Blockchain {
    blockchain_path: String,
    height: usize,
    block_lookup: HashMap<Hash, usize>,
    utxos: UTXOs,
    difficulty_manager: DifficultyManager,
}

impl Blockchain {
    /// Create a new blockchain or load one if exists at blockchain_path
    pub fn new(blockchain_path: &str) -> Self {
        let mut blockchain_path = blockchain_path.to_string();
        if !blockchain_path.ends_with('/') {
            blockchain_path.push('/');
        }
        blockchain_path.push_str("blockchain/");

        if !Path::new(&blockchain_path).exists() {
            fs::create_dir_all(format!("{}blocks/", &blockchain_path)).unwrap();
        }

        match Self::load_cache(&blockchain_path) {
            Ok(cache) => {
                return Blockchain {
                    blockchain_path,
                    height: cache.height,
                    block_lookup: cache.block_lookup,
                    utxos: cache.utxos,
                    difficulty_manager: cache.difficulty_manager,
                };
            }
            Err(_) => {
                return Blockchain {
                    blockchain_path,
                    height: 0,
                    block_lookup: HashMap::new(),
                    utxos: UTXOs::new(),
                    difficulty_manager: DifficultyManager::new(
                        chrono::Utc::now().timestamp() as u64
                    ),
                };
            }
        }
    }

    /// Load the blockchain cache
    fn load_cache(blockchain_path: &str) -> Result<BlockchainData, BlockchainError> {
        let mut file = File::open(format!("{}blockchain.dat", blockchain_path))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        Ok(bincode::decode_from_std_read(&mut file, config::standard())
            .map_err(|e| BlockchainError::BincodeDecode(e.to_string()))?)
    }

    /// Save the blockchain cache
    fn save_cache(&self) -> Result<(), BlockchainError> {
        let mut file = File::create(format!("{}blockchain.dat", self.blockchain_path))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        let cache = BlockchainData {
            difficulty_manager: self.difficulty_manager,
            height: self.height,
            block_lookup: self.block_lookup.clone(),
            utxos: self.utxos.clone(),
        };
        file.sync_all()
            .map_err(|e| BlockchainError::Io(e.to_string()))?;

        bincode::encode_into_std_write(cache, &mut file, config::standard())
            .map_err(|e| BlockchainError::BincodeEncode(e.to_string()))?;
        Ok(())
    }

    fn blocks_dir(&self) -> String {
        format!("{}blocks/", &self.blockchain_path)
    }
    fn block_path_by_height(&self, height: usize) -> String {
        format!("{}{}.dat", self.blocks_dir(), height)
    }
    fn utxo_diffs_path_by_height(&self, height: usize) -> String {
        format!("{}utxo-diffs-{}.dat", self.blocks_dir(), height)
    }
    fn block_path_by_hash(&self, hash: &Hash) -> String {
        format!("{}{}.dat", self.blocks_dir(), self.block_lookup[hash])
    }

    /// Add a block to the blockchain, and then save the state of it
    /// Will return a blockchain error if the block or any of its included transactions are invalid
    pub fn add_block(&mut self, new_block: Block) -> Result<(), BlockchainError> {
        let block_hash = new_block.hash.ok_or(BlockchainError::MissingHash)?;

        if !block_hash.compare_with_data(
            &new_block
                .get_hashing_buf()
                .map_err(|e| BlockchainError::BincodeEncode(e.to_string()))?,
        ) {
            return Err(BlockchainError::HashMismatch {
                expected: Hash::new(
                    &new_block
                        .get_hashing_buf()
                        .map_err(|e| BlockchainError::BincodeEncode(e.to_string()))?,
                )
                .dump_base36(),
                actual: block_hash.dump_base36(),
            });
        }

        if new_block.timestamp > chrono::Utc::now().timestamp() as u64 {
            return Err(BlockchainError::FutureTimestamp(new_block.timestamp));
        }

        if new_block.block_pow_difficulty != self.difficulty_manager.block_difficulty
            || new_block.tx_pow_difficulty != self.difficulty_manager.transaction_difficulty
        {
            return Err(BlockchainError::InvalidDifficulty);
        }

        if self.get_height() == 0 && new_block.previous_block != GENESIS_PREVIOUS_BLOCK_HASH {
            return Err(BlockchainError::InvalidPreviousBlockHash);
        } else if self.get_height() != 0 && *self.get_block_hash_by_height(self.get_height() - 1).unwrap() != new_block.previous_block {
            return Err(BlockchainError::InvalidPreviousBlockHash);
        }

        if BigUint::from_bytes_be(&*block_hash)
            > BigUint::from_bytes_be(&calculate_block_difficulty(
                &self.difficulty_manager.block_difficulty,
                new_block.transactions.len(),
            ))
        {
            return Err(BlockchainError::InvalidDifficulty);
        }

        let mut used_inputs: HashSet<(TransactionId, usize)> = HashSet::new();

        let mut seen_reward_transaction = false;
        for transaction in &new_block.transactions {
            if !transaction.inputs.is_empty() {
                self.utxos.validate_transaction(
                    transaction,
                    &BigUint::from_bytes_be(&self.difficulty_manager.transaction_difficulty),
                )?;

                for input in &transaction.inputs {
                    let key = (input.transaction_id.clone(), input.output_index);
                    if !used_inputs.insert(key.clone()) {
                        return Err(BlockchainError::DoubleSpend);
                    }
                }

                validate_transaction_timestamp_in_block(&transaction, &new_block)?;
            } else {
                if seen_reward_transaction {
                    return Err(BlockchainError::RewardOverspend);
                }
                seen_reward_transaction = true;

                if transaction.outputs.len() < 2 {
                    return Err(BlockchainError::InvalidRewardTransaction);
                }

                if transaction.transaction_id.is_none() {
                    return Err(BlockchainError::RewardTransactionIdMissing);
                }

                if transaction
                    .outputs
                    .iter()
                    .fold(0, |acc, output| acc + output.amount)
                    != get_block_reward(self.height)
                {
                    return Err(BlockchainError::InvalidRewardTransactionAmount);
                }

                let mut has_dev_fee = false;
                for output in &transaction.outputs {
                    if output.receiver == DEV_WALLET
                        && output.amount == calculate_dev_fee(get_block_reward(self.height))
                    {
                        has_dev_fee = true;
                        break;
                    }
                }

                if !has_dev_fee {
                    return Err(BlockchainError::NoDevFee);
                }
            }
        }

        let mut utxo_diffs = UTXODiff::new_empty();

        for transaction in &new_block.transactions {
            utxo_diffs.extend(&mut self.utxos.execute_transaction(transaction));
        }

        self.difficulty_manager.update_difficulty(&new_block);
        self.block_lookup.insert(block_hash, self.height);

        // Encode and save block
        let mut file = File::create(self.block_path_by_height(self.height))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        bincode::encode_into_std_write(new_block, &mut file, config::standard())
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        file.sync_all()
            .map_err(|e| BlockchainError::Io(e.to_string()))?;

        // Encode and save utxo diffs
        let mut file = File::create(self.utxo_diffs_path_by_height(self.height))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        bincode::encode_into_std_write(utxo_diffs, &mut file, config::standard())
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        file.sync_all()
            .map_err(|e| BlockchainError::Io(e.to_string()))?;

        self.height += 1;
        self.save_cache()?;

        Ok(())
    }

    /// Remove the last block added to the blockchain, and update the states of utxos and difficulty manager to return the blockchain to the state it was before the last block was added
    pub fn pop_block(&mut self) -> Result<(), BlockchainError> {
        if self.height == 0 {
            return Err(BlockchainError::NoBlocksToPop);
        }

        let recalled_block = self
            .get_block_by_height(self.height - 1)
            .ok_or(BlockchainError::MissingHash)?;
        let utxo_diffs = self
            .get_utxo_diffs_by_height(self.height - 1)
            .ok_or(BlockchainError::MissingHash)?;

        // Rollback UTXOs
        self.utxos.recall_block_utxos(utxo_diffs);

        // Remove the block file and the utxo diffs file
        fs::remove_file(self.block_path_by_height(self.height - 1))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        fs::remove_file(self.utxo_diffs_path_by_height(self.height - 1))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;

        // Decrement height
        self.height -= 1;

        // Update difficulty manager
        self.difficulty_manager.block_difficulty = recalled_block.block_pow_difficulty;
        self.difficulty_manager.transaction_difficulty = recalled_block.tx_pow_difficulty;
        if self.height > 0
            && let Some(last_block) = self.get_block_by_height(self.height - 1)
        {
            self.difficulty_manager.last_timestamp = last_block.timestamp;
        } else {
            self.difficulty_manager.last_timestamp = recalled_block.timestamp;
        }

        // Save cache
        self.block_lookup.remove(&recalled_block.hash.unwrap());
        self.save_cache()?;

        Ok(())
    }

    pub fn get_block_by_height(&self, height: usize) -> Option<Block> {
        if height >= self.height {
            return None;
        }

        let block_path = self.block_path_by_height(height);
        let mut file = File::open(&block_path).ok()?;
        bincode::decode_from_std_read(&mut file, config::standard()).ok()
    }

    pub fn get_utxo_diffs_by_height(&self, height: usize) -> Option<UTXODiff> {
        if height >= self.height {
            return None;
        }

        let diffs_path = self.utxo_diffs_path_by_height(height);
        let mut file = File::open(&diffs_path).ok()?;
        bincode::decode_from_std_read(&mut file, config::standard()).ok()
    }

    pub fn get_block_by_hash(&self, hash: &Hash) -> Option<Block> {
        let block_path = self.block_path_by_hash(hash);
        let mut file = File::open(&block_path).ok()?;
        bincode::decode_from_std_read(&mut file, config::standard()).ok()
    }

    pub fn get_height_by_hash(&self, hash: &Hash) -> Option<usize> {
        self.block_lookup.get(hash).copied()
    }

    pub fn get_block_hash_by_height(&self, height: usize) -> Option<&Hash> {
        match self.block_lookup.iter().find(|x| x.1 == &height) {
            Some(blt) => Some(blt.0),
            None => None,
        }
    }

    pub fn get_height(&self) -> usize {
        self.height
    }

    pub fn get_utxos(&self) -> &UTXOs {
        &self.utxos
    }

    pub fn get_difficulty_manager(&self) -> &DifficultyManager {
        &self.difficulty_manager
    }

    pub fn get_transaction_difficulty(&self) -> [u8; 32] {
        self.difficulty_manager.transaction_difficulty
    }

    pub fn get_block_difficulty(&self) -> [u8; 32] {
        self.difficulty_manager.block_difficulty
    }

    pub fn get_all_blocks(&self) -> Vec<&Hash> {
        self.block_lookup.keys().collect()
    }
}

#[async_trait::async_trait]
impl BlockchainDataProvider for Blockchain {
    async fn get_height(
        &self,
    ) -> Result<usize, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_height())
    }

    async fn get_reward(
        &self,
    ) -> Result<u64, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(get_block_reward(self.get_height()))
    }

    async fn get_block_by_height(
        &self,
        height: usize,
    ) -> Result<Option<Block>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_block_by_height(height))
    }

    async fn get_block_by_hash(
        &self,
        hash: &Hash,
    ) -> Result<Option<Block>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_block_by_hash(hash))
    }

    async fn get_height_by_hash(
        &self,
        hash: &Hash,
    ) -> Result<Option<usize>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_height_by_hash(hash))
    }

    async fn get_block_hash_by_height(
        &self,
        height: usize,
    ) -> Result<Option<Hash>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_block_hash_by_height(height).copied())
    }

    async fn get_transaction_difficulty(
        &self,
    ) -> Result<[u8; 32], crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_transaction_difficulty())
    }

    async fn get_block_difficulty(
        &self,
    ) -> Result<[u8; 32], crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_block_difficulty())
    }

    async fn get_available_transaction_outputs(
        &self,
        address: crate::crypto::keys::Public,
    ) -> Result<
        Vec<(TransactionId, super::transaction::TransactionOutput, usize)>,
        crate::blockchain_data_provider::BlockchainDataProviderError,
    > {
        let mut available_outputs = vec![];

        for (transaction_id, outputs) in self.utxos.utxos.iter() {
            for (output_index, output) in outputs.iter().enumerate() {
                if let Some(output) = output
                    && output.receiver == address
                {
                    available_outputs.push((*transaction_id, output.clone(), output_index));
                }
            }
        }

        Ok(available_outputs)
    }
}

/// Returns true if transaction timestamp is valid in the context of a block
pub fn validate_transaction_timestamp_in_block(
    transaction: &Transaction,
    owning_block: &Block,
) -> Result<(), BlockchainError> {
    if transaction.timestamp > owning_block.timestamp {
        return Err(BlockchainError::InvalidTimestamp);
    }
    if transaction.timestamp + EXPIRATION_TIME < owning_block.timestamp {
        return Err(BlockchainError::InvalidTimestamp);
    }

    Ok(())
}

/// Returns true if transaction timestamp is valid in the context of current time
pub fn validate_transaction_timestamp(transaction: &Transaction) -> Result<(), BlockchainError> {
    if transaction.timestamp > chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }
    if transaction.timestamp + EXPIRATION_TIME < chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }

    Ok(())
}

/// Returns false if block timestamp is valid
pub fn validate_block_timestamp(block: &Block) -> Result<(), BlockchainError> {
    if block.timestamp > chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }
    if block.timestamp + EXPIRATION_TIME < chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }

    Ok(())
}
