use std::{
    collections::HashSet,
    fs::{self, File},
    path::Path,
};

use bincode::{Decode, Encode};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    core::{
        block::{Block, BlockError, MAX_TRANSACTIONS_PER_BLOCK},
        block_store::{BlockStore, BlockStoreError},
        difficulty::DifficultyState,
        transaction::{Transaction, TransactionError, TransactionId},
        utxo::{UTXODiff, UTXOs},
    },
    crypto::Hash,
    economics::{DEV_WALLET, EXPIRATION_TIME, calculate_dev_fee, get_block_reward},
};

#[derive(Error, Debug, Serialize, Deserialize, Clone, Encode, Decode)]
pub enum BlockchainError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("Bincode decode error: {0}")]
    BincodeDecode(String),

    #[error("Bincode encode error: {0}")]
    BincodeEncode(String),

    #[error("Block does not have a hash attached")]
    IncompleteBlock,

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

    #[error("Block to pop was not found")]
    BlockNotFound,

    #[error("Block or transaction timestamp is invalid or expired")]
    InvalidTimestamp,

    #[error("Previous block hash is invalid")]
    InvalidPreviousBlockHash,

    #[error("Block has too many transactions")]
    TooManyTransactions,

    #[error("Block error: {0}")]
    Block(#[from] BlockError),

    #[error("Block store error: {0}")]
    BlockStore(#[from] BlockStoreError),

    #[error("UTXOs error: {0}")]
    UTXOs(String),

    #[error("Live transaction difficulty not beat")]
    LiveTransactionDifficulty,
}

impl From<TransactionError> for BlockchainError {
    fn from(err: TransactionError) -> Self {
        BlockchainError::InvalidTransaction(err.to_string())
    }
}

/// BlockchainData is an object used for storing and loading current blockchain state.
#[derive(Encode, Decode, Debug, Clone)]
pub struct BlockchainData {
    pub difficulty_state: DifficultyState,
    pub height: usize,
    pub last_block: Hash,
}

/// The blockchain, handles everything. Core to this crypto coin.
pub struct Blockchain {
    blockchain_path: String,
    block_store: BlockStore,
    utxos: UTXOs,
    difficulty_state: DifficultyState,
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

        match Self::load_blockchain_data(&blockchain_path) {
            Ok(blockchain_data) => {
                return Blockchain {
                    block_store: BlockStore::load(
                        &format!("{}blocks/", blockchain_path),
                        blockchain_data.height,
                        blockchain_data.last_block,
                    ),
                    utxos: UTXOs::new(blockchain_path.clone()),
                    difficulty_state: blockchain_data.difficulty_state,
                    blockchain_path,
                };
            }
            Err(_) => {
                return Blockchain {
                    utxos: UTXOs::new(blockchain_path.clone()),
                    difficulty_state: DifficultyState::new_default(),
                    block_store: BlockStore::new_empty(&format!("{}blocks/", blockchain_path)),
                    blockchain_path,
                };
            }
        }
    }

    /// Load the blockchain data
    pub fn load_blockchain_data(blockchain_path: &str) -> Result<BlockchainData, BlockchainError> {
        let mut file = File::open(format!("{}blockchain-info.dat", blockchain_path))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        Ok(
            bincode::decode_from_std_read(&mut file, bincode::config::standard())
                .map_err(|e| BlockchainError::BincodeDecode(e.to_string()))?,
        )
    }

    /// Save the blockchain data
    pub fn save_blockchain_data(
        blockchain_path: &str,
        difficulty_state: DifficultyState,
        height: usize,
        last_block: Hash,
    ) -> Result<(), BlockchainError> {
        let mut file = File::create(format!("{}blockchain-info.dat", blockchain_path))
            .map_err(|e| BlockchainError::Io(e.to_string()))?;
        let blockchain_data = BlockchainData {
            difficulty_state,
            height,
            last_block,
        };
        file.sync_all()
            .map_err(|e| BlockchainError::Io(e.to_string()))?;

        bincode::encode_into_std_write(blockchain_data, &mut file, bincode::config::standard())
            .map_err(|e| BlockchainError::BincodeEncode(e.to_string()))?;
        Ok(())
    }

    /// Add a block to the blockchain, and then save the state of it
    /// Will return a blockchain error if the block or any of its included transactions are invalid
    /// WARNING: This can be randomly async unsafe, so if you have problems, wrap it in a spawn_blocking()
    pub fn add_block(&self, new_block: Block, is_ibd: bool) -> Result<(), BlockchainError> {
        new_block.check_meta()?;

        new_block.validate_difficulties(
            &self.get_block_difficulty(),
            &self.get_transaction_difficulty(),
        )?;

        // Validate previous hash
        if self.block_store.get_last_block_hash() != new_block.meta.previous_block {
            return Err(BlockchainError::InvalidPreviousBlockHash);
        }

        // Check if we don't have too many TXs
        if new_block.transactions.len() > MAX_TRANSACTIONS_PER_BLOCK {
            return Err(BlockchainError::TooManyTransactions);
        }

        let mut used_inputs: HashSet<(TransactionId, usize)> = HashSet::new();

        let mut seen_reward_transaction = false;
        for transaction in &new_block.transactions {
            if !transaction.inputs.is_empty() {
                // Normal tx
                // Validate transaction in context of UTXOs
                self.utxos.validate_transaction(
                    transaction,
                    &BigUint::from_bytes_be(&self.get_transaction_difficulty()),
                    is_ibd,
                )?;

                // Check for double spending
                for input in &transaction.inputs {
                    let key = (input.transaction_id, input.output_index);
                    if !used_inputs.insert(key.clone()) {
                        return Err(BlockchainError::DoubleSpend);
                    }
                }

                validate_transaction_timestamp_in_block(&transaction, &new_block)?;
            } else {
                // Reward tx
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
                    != get_block_reward(self.block_store().get_height())
                {
                    return Err(BlockchainError::InvalidRewardTransactionAmount);
                }

                let mut has_dev_fee = false;
                for output in &transaction.outputs {
                    if output.receiver == DEV_WALLET
                        && output.amount
                            == calculate_dev_fee(get_block_reward(self.block_store().get_height()))
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

        // Calculate and execute all utxo diffs
        let mut utxo_diffs = UTXODiff::new_empty();

        for transaction in &new_block.transactions {
            utxo_diffs.extend(&mut self.utxos.execute_transaction(transaction)?);
        }

        self.difficulty_state.update_difficulty(&new_block);

        self.block_store().add_block(new_block, utxo_diffs)?;
        Self::save_blockchain_data(
            &self.blockchain_path,
            self.difficulty_state.clone(),
            self.block_store().get_height(),
            self.block_store().get_last_block_hash(),
        )?;

        Ok(())
    }

    /// Remove the last block added to the blockchain, and update the states of utxos and difficulty manager to return the blockchain to the state it was before the last block was added
    pub fn pop_block(&self) -> Result<(), BlockchainError> {
        if self.block_store().get_height() == 0 {
            return Err(BlockchainError::NoBlocksToPop);
        }

        let recalled_block = self
            .block_store()
            .get_last_block()
            .ok_or(BlockchainError::BlockNotFound)?;
        let utxo_diffs = self
            .block_store()
            .get_last_utxo_diffs()
            .ok_or(BlockchainError::BlockNotFound)?;

        // Rollback UTXOs
        self.utxos.recall_block_utxos(&utxo_diffs)?;

        self.block_store().pop_block()?;

        // Update difficulty manager
        *self.difficulty_state.block_difficulty.write().unwrap() =
            recalled_block.meta.block_pow_difficulty;
        *self
            .difficulty_state
            .transaction_difficulty
            .write()
            .unwrap() = recalled_block.meta.tx_pow_difficulty;
        if self.block_store().get_height() > 0
            && let Some(last_block) = self.block_store().get_last_block()
        {
            *self.difficulty_state.last_timestamp.write().unwrap() = last_block.timestamp;
        } else {
            *self.difficulty_state.last_timestamp.write().unwrap() = recalled_block.timestamp;
        }

        // Save blockchain data
        Self::save_blockchain_data(
            &self.blockchain_path,
            self.difficulty_state.clone(),
            self.block_store().get_height(),
            self.block_store().get_last_block_hash(),
        )?;

        Ok(())
    }

    pub fn get_utxos(&self) -> &UTXOs {
        &self.utxos
    }

    pub fn get_difficulty_manager(&self) -> &DifficultyState {
        &self.difficulty_state
    }

    pub fn get_transaction_difficulty(&self) -> [u8; 32] {
        *self.difficulty_state.transaction_difficulty.read().unwrap()
    }

    pub fn get_block_difficulty(&self) -> [u8; 32] {
        *self.difficulty_state.block_difficulty.read().unwrap()
    }

    pub fn block_store(&self) -> &BlockStore {
        &self.block_store
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
    if transaction.timestamp - EXPIRATION_TIME > chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }
    if transaction.timestamp + EXPIRATION_TIME < chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }

    Ok(())
}

/// Returns false if block timestamp is valid
pub fn validate_block_timestamp(block: &Block) -> Result<(), BlockchainError> {
    if block.timestamp - EXPIRATION_TIME > chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }
    if block.timestamp + EXPIRATION_TIME < chrono::Utc::now().timestamp() as u64 {
        return Err(BlockchainError::InvalidTimestamp);
    }

    Ok(())
}
