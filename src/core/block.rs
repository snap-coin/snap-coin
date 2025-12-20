use bincode::{Decode, Encode, error::EncodeError};
use num_bigint::BigUint;
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    core::{difficulty::calculate_block_difficulty, transaction::Transaction},
    crypto::Hash,
};

pub const MAX_TRANSACTIONS: usize = 500;

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum BlockError {
    #[error("Block is missing required metadata")]
    IncompleteBlock,

    #[error("Encoding error")]
    EncodeError,

    #[error("Block hash is invalid")]
    InvalidBlockHash,

    #[error("Block difficulties don't match real difficulties")]
    DifficultyMismatch,

    #[error("Block pow difficulty is not up to target")]
    BlockPowDifficultyIncorrect,
}

/// Stores transaction, difficulties, its hash, and its nonce
/// The hash can be often used for indexing, however can only be trusted if this node checked this block already
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub transactions: Vec<Transaction>,
    pub timestamp: u64,
    pub nonce: u64,
    pub meta: BlockMetadata,
}

impl Block {
    /// Create a new block timestamped now, with a set of transactions, specifying transaction difficulty and block difficulty
    pub fn new_block_now(
        transactions: Vec<Transaction>,
        block_pow_difficulty: &[u8; 32],
        tx_pow_difficulty: &[u8; 32],
        previous_block: Hash,
    ) -> Self {
        Block {
            transactions,
            timestamp: chrono::Utc::now().timestamp() as u64,
            nonce: 0,
            meta: BlockMetadata {
                block_pow_difficulty: *block_pow_difficulty,
                tx_pow_difficulty: *tx_pow_difficulty,
                previous_block,
                hash: None,
            },
        }
    }

    /// Get this blocks hashing buffer required to mine this transaction. Essentially makes sure that any hash attached to this block is not included in the block hashing buffer
    /// WARNING: Slow
    pub fn get_hashing_buf(&self) -> Result<Vec<u8>, EncodeError> {
        let mut hash_less_block = self.clone();
        hash_less_block.meta.hash = None; // Remove hash

        // Remove all transaction inputs because, we can just hash the transaction hash and keep the integrity
        for transaction in &mut hash_less_block.transactions {
            transaction.inputs = vec![];
            transaction.outputs = vec![];
        }
        bincode::encode_to_vec(hash_less_block, bincode::config::standard())
    }

    /// Mine this block and attach its hash.
    /// DEPRECATED: This is single threaded and cannot be used for actual mining as proper, multi-threaded mining machines outperform this by absolute miles
    #[deprecated]
    pub fn compute_pow(&mut self) -> Result<(), EncodeError> {
        let tx_difficulty_big_int = BigUint::from_bytes_be(&calculate_block_difficulty(
            &self.meta.block_pow_difficulty,
            self.transactions.len(),
        ));
        let mut rng: rand::prelude::ThreadRng = rand::rng();
        loop {
            self.nonce = rng.random();
            let hashing_buf = self.get_hashing_buf()?;
            if BigUint::from_bytes_be(&*Hash::new(&hashing_buf)) <= tx_difficulty_big_int {
                self.meta.hash = Some(Hash::new(&hashing_buf));
                return Ok(());
            }
        }
    }

    /// Checks if block meta is valid
    pub fn check_meta(&self) -> Result<(), BlockError> {
        self.check_completeness()?;
        self.validate_block_hash()?;
        self.validate_block_hash()?;
        Ok(())
    }

    /// Checks if this block is complete, and has all required fields to be valid on a blockchain
    pub fn check_completeness(&self) -> Result<(), BlockError> {
        self.meta
            .hash
            .ok_or(BlockError::IncompleteBlock)
            .map(|_| ())?;
        Ok(())
    }

    /// Checks if the attached block hash is valid
    pub fn validate_block_hash(&self) -> Result<(), BlockError> {
        self.check_completeness()?;
        if !self
            .meta
            .hash
            .ok_or(BlockError::IncompleteBlock)?
            .compare_with_data(
                &self
                    .get_hashing_buf()
                    .map_err(|_| BlockError::EncodeError)?,
            )
        {
            return Err(BlockError::InvalidBlockHash);
        }
        Ok(())
    }

    /// Checks if the passed difficulties match the blocks difficulties (true = valid, false = invalid)
    pub fn validate_difficulties(
        &self,
        real_block_pow_difficulty: &[u8; 32],
        real_tx_pow_difficulty: &[u8; 32],
    ) -> Result<(), BlockError> {
        if self.meta.block_pow_difficulty != *real_block_pow_difficulty
            || self.meta.tx_pow_difficulty != *real_tx_pow_difficulty
        {
            return Err(BlockError::DifficultyMismatch);
        }
        if BigUint::from_bytes_be(&*self.meta.hash.unwrap())
            > BigUint::from_bytes_be(&calculate_block_difficulty(
                real_block_pow_difficulty,
                self.transactions.len(),
            ))
        {
            return Err(BlockError::BlockPowDifficultyIncorrect);
        }
        Ok(())
    }
}

// Represents all block data that is not essential to it's existence (however required)
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug)]
pub struct BlockMetadata {
    pub block_pow_difficulty: [u8; 32],
    pub tx_pow_difficulty: [u8; 32],
    pub previous_block: Hash,
    pub hash: Option<Hash>,
}
