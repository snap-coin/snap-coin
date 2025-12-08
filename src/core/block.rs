use bincode::{Decode, Encode, error::EncodeError};
use num_bigint::BigUint;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::{
    core::{difficulty::calculate_block_difficulty, transaction::Transaction},
    crypto::Hash,
};

/// Stores transaction, difficulties, its hash, and its nonce
/// hash can be often used for indexing, however can only be trusted if this node checked this block already
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub transactions: Vec<Transaction>,
    pub timestamp: u64,
    pub nonce: u64,
    pub block_pow_difficulty: [u8; 32],
    pub tx_pow_difficulty: [u8; 32],
    pub previous_block: Hash,
    pub hash: Option<Hash>,
}

impl Block {
    /// Create a new block timestamped now, with a set of transactions, specifying transaction difficulty and block difficulty
    pub fn new_block_now(
        transactions: Vec<Transaction>,
        block_pow_difficulty: &[u8; 32],
        tx_pow_difficulty: &[u8; 32],
        previous_block: Hash
    ) -> Self {
        Block {
            transactions,
            timestamp: chrono::Utc::now().timestamp() as u64,
            nonce: 0,
            block_pow_difficulty: *block_pow_difficulty,
            tx_pow_difficulty: *tx_pow_difficulty,
            previous_block,
            hash: None,
        }
    }

    /// Get this blocks hashing buffer required to mine this transaction. Essentially makes sure that any hash attached to this block is not included in the block hashing buffer
    pub fn get_hashing_buf(&self) -> Result<Vec<u8>, EncodeError> {
        let mut hash_less_block = self.clone();
        hash_less_block.hash = None;
        bincode::encode_to_vec(hash_less_block, bincode::config::standard())
    }

    /// Mine this block and attach its hash.
    /// DEPRECATED: This is single threaded and cannot be used for actual mining as proper, multi-threaded mining machines outperform this by absolute miles
    #[deprecated]
    pub fn compute_pow(&mut self) -> Result<(), EncodeError> {
        let tx_difficulty_big_int = BigUint::from_bytes_be(&calculate_block_difficulty(
            &self.block_pow_difficulty,
            self.transactions.len(),
        ));
        let mut rng: rand::prelude::ThreadRng = rand::rng();
        loop {
            self.nonce = rng.random();
            let hashing_buf = self.get_hashing_buf()?;
            if BigUint::from_bytes_be(&*Hash::new(&hashing_buf)) <= tx_difficulty_big_int {
                self.hash = Some(Hash::new(&hashing_buf));
                return Ok(());
            }
        }
    }
}
