use crate::{
    core::block::MAX_TRANSACTIONS_PER_BLOCK,
    crypto::{Hash, keys::Public},
};

/// Convert NANO amount (internal way of storing funds as u64s), smallest grain of one coin possible to avoid
pub const NANO_TO_SNAP: f64 = 100_000_000f64;

/// Initial amount rewarded to miner (also split with devs by dev fee)
pub const INITIAL_REWARD: u64 = to_nano(100.0);

/// Max amount the difficulty can change per block
pub const MAX_TX_DIFF_CHANGE: f64 = 0.8;

/// Target time in seconds for each block
pub const TARGET_TIME: u64 = 35;

/// Target amount of transactions per block
pub const TX_TARGET: usize = 100;

/// How slowly difficulty moves (bigger = slower changes)
pub const DIFFICULTY_ADJUST_DIVISOR: f64 = 64.0;

/// Max percent change per block (±)
pub const MAX_BLOCK_DIFFICULTY_ADJUST: f64 = 0.05;

/// Minimum block difficulty (floor)
pub const MIN_BLOCK_DIFFICULTY: [u8; 32] = [0u8; 32];

/// Precision scaler for ratio math
pub const DIFF_ADJUST_SCALE: u64 = 1_000_000;

/// Halving of reward happens every how many blocks
pub const HALVING_INTERVAL: usize = 1_000_000; // number of blocks per halving

/// Minimum possible reward
pub const MIN_REWARD: u64 = 1; // smallest possible reward

/// Developer wallet address
pub const DEV_WALLET: Public = Public::new_from_buf(&[
    237, 16, 162, 56, 254, 203, 62, 193, 77, 162, 64, 178, 25, 226, 137, 184, 77, 191, 219, 2, 54,
    178, 222, 164, 139, 138, 195, 169, 96, 66, 159, 155,
]);
/// Developer fee percent
pub const DEV_FEE: f64 = 0.02;

/// Percent of difficulty decayed per every transaction included in block
pub const DIFFICULTY_DECAY_PER_TRANSACTION: f64 = 0.005;

/// Percent by which the transaction difficulty is increased (compound) per tx already in mempool
pub const MEMPOOL_PRESSURE_PER_TRANSACTION: f64 = 1f64 / (MAX_TRANSACTIONS_PER_BLOCK as f64);

/// Transaction expiration time
pub const EXPIRATION_TIME: u64 = TARGET_TIME * 10;

/// Genesis previous block hash
pub const GENESIS_PREVIOUS_BLOCK_HASH: Hash = Hash::new_from_buf([0u8; 32]);

/// Convert NANO amount to SNAP (rounded to nearest)
/// WARNING: LOSSY
pub const fn to_snap(nano: u64) -> f64 {
    nano as f64 / NANO_TO_SNAP
}

/// Convert SNAP amount to NANO (rounded to nearest u64)
pub const fn to_nano(snap: f64) -> u64 {
    (snap * NANO_TO_SNAP).round() as u64
}

/// Block reward halves every `HALVING_INTERVAL` blocks
pub fn get_block_reward(height: usize) -> u64 {
    let halvings = height / HALVING_INTERVAL;
    let reward = INITIAL_REWARD >> halvings; // Divide by 2^halvings
    reward.max(MIN_REWARD)
}

/// Total reward up to a given block height (exclusive)
pub fn total_reward(up_to_height: usize) -> u64 {
    let mut total = 0u64;
    for height in 0..up_to_height {
        total += get_block_reward(height);
    }
    total
}

/// Calculate dev fee taken from a block reward
pub const fn calculate_dev_fee(block_reward: u64) -> u64 {
    (block_reward as f64 * DEV_FEE) as u64
}

// Snap Coin Improvement Protocol migration dates
pub const SCIP_1_MIGRATION: u64 = 1770375600; // February 6, 2026 12:00:00 AM CET
pub const SCIP_2_MIGRATION: u64 = 1770894000; // Thursday Feb 12 2026 12:00:00 CET
