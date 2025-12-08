use crate::core::{
    block::Block,
    economics::{DIFFICULTY_DECAY_PER_TX, MAX_DIFF_CHANGE, TARGET_TIME, TX_TARGET},
    utils::{clamp_f, max_256_bui},
};
use bincode::{Decode, Encode};
use num_bigint::BigUint;

pub const STARTING_BLOCK_DIFFICULTY: [u8; 32] = [u8::MAX; 32];
pub const STARTING_TX_DIFFICULTY: [u8; 32] = [u8::MAX; 32];

#[derive(Encode, Decode, Copy, Clone, Debug)]
pub struct DifficultyManager {
    pub block_difficulty: [u8; 32],
    pub transaction_difficulty: [u8; 32],
    pub last_timestamp: u64,
}

/// Manages network difficulty and TX POW difficulty
impl DifficultyManager {
    /// Create a new empty Difficulty Manager timestamped now
    pub fn new(last_timestamp: u64) -> Self {
        DifficultyManager {
            block_difficulty: STARTING_BLOCK_DIFFICULTY,
            transaction_difficulty: STARTING_TX_DIFFICULTY,
            last_timestamp,
        }
    }

    /// Update the network difficulties after adding a new block to the blockchain
    pub fn update_difficulty(&mut self, new_block: &Block) {
        // Block difficulty
        let time_ratio = (clamp_f(
            (new_block.timestamp - self.last_timestamp) as f64 / TARGET_TIME as f64,
            MAX_DIFF_CHANGE,
            2.0 - MAX_DIFF_CHANGE,
        ) * 1000.0) as u64;

        let mut block_big = BigUint::from_bytes_be(&self.block_difficulty);
        block_big = block_big * BigUint::from(time_ratio) / BigUint::from(1000u64);
        self.block_difficulty =
            biguint_to_32_bytes(block_big.min(max_256_bui()).max(BigUint::ZERO));

        // Transaction difficulty
        let tx_ratio = (clamp_f(
            TX_TARGET as f64 / new_block.transactions.len() as f64,
            MAX_DIFF_CHANGE,
            2.0 - MAX_DIFF_CHANGE,
        ) * 1000.0) as u64;

        let mut tx_big = BigUint::from_bytes_be(&self.transaction_difficulty);
        tx_big = tx_big * BigUint::from(tx_ratio) / BigUint::from(1000u64);
        self.transaction_difficulty = biguint_to_32_bytes(tx_big.min(max_256_bui()).max(BigUint::ZERO));

        // Update last timestamp
        self.last_timestamp = new_block.timestamp;
    }
}

/// Calculate blockchain block difficulty transaction decay based on the current, base difficulty and amount of transactions in block
pub fn calculate_block_difficulty(block_difficulty: &[u8; 32], tx_count: usize) -> [u8; 32] {
    let big = BigUint::from_bytes_be(block_difficulty);
    biguint_to_32_bytes(
        (big * BigUint::from(((1f64 + DIFFICULTY_DECAY_PER_TX * tx_count as f64) * 1000f64) as u64)
            / BigUint::from(1000u64)).min(max_256_bui()),
    )
}

fn biguint_to_32_bytes(value: BigUint) -> [u8; 32] {
    let mut bytes = value.to_bytes_be();
    if bytes.len() < 32 {
        let mut padded = vec![0u8; 32 - bytes.len()];
        padded.extend(bytes);
        bytes = padded;
    } else if bytes.len() > 32 {
        bytes = bytes[bytes.len() - 32..].to_vec();
    }
    let mut array = [0u8; 32];
    array.copy_from_slice(&bytes);
    array
}
