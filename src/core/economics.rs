use crate::crypto::{Hash, keys::Public};

/// Convert NANO amount (internal way of storing funds as u64s), smallest grain of one coin possible to avoid
pub const NANO_TO_SNAP: f64 = 100_000_000f64;

/// Initial amount rewarded to miner (also split with devs by dev fee)
pub const INITIAL_REWARD: u64 = to_nano(100.0);

/// Target time in seconds for each block
pub const TARGET_TIME: u64 = 20;

/// Target amount of transactions per block
pub const TX_TARGET: usize = 100;

/// Max amount the difficulty can change per block (TX and block diff)
pub const MAX_DIFF_CHANGE: f64 = 0.8;

/// Halving of reward happens every how many blocks
pub const HALVING_INTERVAL: usize = 210_000; // number of blocks per halving

/// Minimum possible reward
pub const MIN_REWARD: u64 = 1; // smallest possible reward

/// Developer wallet address
pub const DEV_WALLET: Public = Public::new_from_buf(&[
    234u8, 96u8, 97u8, 87u8, 97u8, 239u8, 56u8, 52u8, 234u8, 43u8, 146u8, 76u8, 74u8, 153u8, 196u8,
    117u8, 237u8, 99u8, 76u8, 101u8, 164u8, 71u8, 29u8, 247u8, 192u8, 124u8, 101u8, 198u8, 234u8,
    19u8, 244u8, 157u8,
]);
/// Developer fee percent
pub const DEV_FEE: f64 = 0.02;

/// Percent of difficulty decayed per every transaction included in block
pub const DIFFICULTY_DECAY_PER_TX: f64 = 0.005;

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
