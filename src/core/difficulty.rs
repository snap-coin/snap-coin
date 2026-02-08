use std::sync::RwLock;

use crate::{
    core::{
        block::Block,
        economics::{DIFFICULTY_DECAY_PER_TRANSACTION, TARGET_TIME, TX_TARGET},
        utils::{clamp_f, max_256_bui},
    },
    economics::{
        DIFF_ADJUST_SCALE, DIFFICULTY_ADJUST_DIVISOR, MAX_BLOCK_DIFFICULTY_ADJUST,
        MAX_TX_DIFF_CHANGE, MEMPOOL_PRESSURE_PER_TRANSACTION, MIN_BLOCK_DIFFICULTY,
        SCIP_2_MIGRATION,
    },
};
use bincode::{Decode, Encode};
use num_bigint::BigUint;

pub const STARTING_BLOCK_DIFFICULTY: [u8; 32] = [u8::MAX; 32];
pub const STARTING_TX_DIFFICULTY: [u8; 32] = [u8::MAX; 32];

#[derive(Encode, Decode, Debug)]
pub struct DifficultyState {
    pub block_difficulty: RwLock<[u8; 32]>,
    pub transaction_difficulty: RwLock<[u8; 32]>,
    pub last_timestamp: RwLock<u64>,
}

/// Manages network difficulty and TX POW difficulty
impl DifficultyState {
    /// Create a new empty Difficulty State
    pub fn new_default() -> Self {
        DifficultyState {
            block_difficulty: RwLock::new(STARTING_BLOCK_DIFFICULTY),
            transaction_difficulty: RwLock::new(STARTING_TX_DIFFICULTY),
            last_timestamp: RwLock::new(0),
        }
    }

    /// Update the network difficulties after adding a new block to the blockchain
    pub fn update_difficulty(&self, new_block: &Block) {
        if new_block.timestamp > SCIP_2_MIGRATION {
            // Block difficulty post SCIP-2
            let last_ts = *self.last_timestamp.read().unwrap();
            let now_ts = new_block.timestamp;

            let actual_time = now_ts.saturating_sub(last_ts).max(1) as f64;
            let target_time = TARGET_TIME as f64;

            // Positive if blocks are fast, negative if slow
            let error = (target_time - actual_time) / target_time;

            // Small step
            let mut adj = error / DIFFICULTY_ADJUST_DIVISOR as f64;

            // Bound it
            adj = adj.clamp(-MAX_BLOCK_DIFFICULTY_ADJUST, MAX_BLOCK_DIFFICULTY_ADJUST);

            // Invert for target math
            let ratio = 1.0 - adj;

            // Scale to avoid float BigUint
            let scaled = (ratio * DIFF_ADJUST_SCALE as f64) as u64;

            let mut target = BigUint::from_bytes_be(&*self.block_difficulty.read().unwrap());

            target = target * BigUint::from(scaled) / BigUint::from(DIFF_ADJUST_SCALE);

            let min = BigUint::from_bytes_be(&MIN_BLOCK_DIFFICULTY);
            let max = max_256_bui();

            target = target.max(min).min(max);

            *self.block_difficulty.write().unwrap() = biguint_to_32_bytes(target.clone());
        } else {
            // Block difficulty pre SCIP-2
            let time_ratio = (clamp_f(
                (new_block
                    .timestamp
                    .saturating_sub(*self.last_timestamp.read().unwrap())) as f64
                    / 20 as f64,
                0.8f64,
                2.0 - 0.8f64,
            ) * 1000.0) as u64;

            let mut block_big = BigUint::from_bytes_be(&*self.block_difficulty.read().unwrap());
            block_big = block_big * BigUint::from(time_ratio) / BigUint::from(1000u64);
            *self.block_difficulty.write().unwrap() =
                biguint_to_32_bytes(block_big.min(max_256_bui()).max(BigUint::ZERO));
        }

        // Transaction Difficulty
        let tx_ratio = (clamp_f(
            TX_TARGET as f64 / new_block.transactions.len().max(1) as f64,
            MAX_TX_DIFF_CHANGE,
            2.0 - MAX_TX_DIFF_CHANGE,
        ) * 1000.0) as u64;

        let mut tx_big = BigUint::from_bytes_be(&*self.transaction_difficulty.read().unwrap());
        tx_big = tx_big * BigUint::from(tx_ratio) / BigUint::from(1000u64);
        *self.transaction_difficulty.write().unwrap() =
            biguint_to_32_bytes(tx_big.min(max_256_bui()).max(BigUint::ZERO));

        // ---------------- TIMESTAMP ----------------

        *self.last_timestamp.write().unwrap() = new_block.timestamp;
    }

    pub fn get_block_difficulty(&self) -> [u8; 32] {
        *self.block_difficulty.read().unwrap()
    }

    pub fn get_transaction_difficulty(&self) -> [u8; 32] {
        *self.transaction_difficulty.read().unwrap()
    }
}

impl Clone for DifficultyState {
    fn clone(&self) -> Self {
        Self {
            block_difficulty: RwLock::new(*self.block_difficulty.read().unwrap()),
            transaction_difficulty: RwLock::new(*self.transaction_difficulty.read().unwrap()),
            last_timestamp: RwLock::new(*self.last_timestamp.read().unwrap()),
        }
    }
}

/// Calculate blockchain block difficulty transaction decay based on the current, base difficulty and amount of transactions in block
pub fn calculate_block_difficulty(block_difficulty: &[u8; 32], tx_count: usize) -> [u8; 32] {
    let difficulty = BigUint::from_bytes_be(block_difficulty);
    biguint_to_32_bytes(
        (difficulty
            * BigUint::from(
                ((1f64 + DIFFICULTY_DECAY_PER_TRANSACTION * tx_count as f64) * 1000f64) as u64,
            )
            / BigUint::from(1000u64))
        .min(max_256_bui()),
    )
}

/// Calculate blockchain block difficulty transaction decay based on the current, base difficulty and amount of transactions in block
pub fn calculate_live_transaction_difficulty(
    transaction_difficulty: &[u8; 32],
    mempool_size: usize,
) -> [u8; 32] {
    let difficulty = BigUint::from_bytes_be(transaction_difficulty);

    // Calculate the decay factor
    let decay = (mempool_size as f64) * MEMPOOL_PRESSURE_PER_TRANSACTION;
    let decay = decay.clamp(0.0, 1.0); // Ensure it stays between 0 and 1

    // Apply linear decay: new_difficulty = original * (1 - decay)
    let factor = (1.0 - decay) * 1_000_000.0; // scale to avoid float rounding issues
    let mut new_difficulty = &difficulty * BigUint::from(factor as u64);
    new_difficulty /= BigUint::from(1_000_000u64);

    biguint_to_32_bytes(new_difficulty)
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
