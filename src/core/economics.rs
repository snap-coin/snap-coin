use crate::core::keys::Public;

pub const NANO_TO_SNAP: u64 = 100_000_000u64;
pub const INITIAL_REWARD: u64 = to_nano(100);
pub const TARGET_TIME: u64 = 20;
pub const TX_TARGET: usize = 100;
pub const MAX_DIFF_CHANGE: f64 = 0.8;
pub const HALVING_INTERVAL: usize = 210_000; // number of blocks per halving
pub const MIN_REWARD: u64 = 1; // smallest possible reward
pub const DEV_WALLET: Public = Public::new_from_buf(&[234u8, 96u8, 97u8, 87u8, 97u8, 239u8, 56u8, 52u8, 234u8, 43u8, 146u8, 76u8, 74u8, 153u8, 196u8, 117u8, 237u8, 99u8, 76u8, 101u8, 164u8, 71u8, 29u8, 247u8, 192u8, 124u8, 101u8, 198u8, 234u8, 19u8, 244u8, 157u8]);
pub const DEV_FEE: f64 = 0.05;
pub const DIFFICULTY_DECAY_PER_TX: f64 = 0.95;

pub const fn to_snap(nano: u64) -> u64 {
  (nano + NANO_TO_SNAP / 2) / NANO_TO_SNAP
}

pub const fn to_nano(snap: u64) -> u64 {
  snap * NANO_TO_SNAP
}

// Block reward halves every `HALVING_INTERVAL` blocks
pub fn get_block_reward(height: usize) -> u64 {
  let halvings = height / HALVING_INTERVAL;
  let reward = INITIAL_REWARD >> halvings; // divide by 2^halvings
  reward.max(MIN_REWARD)
}

// Total reward up to a given block height (exclusive)
pub fn total_reward(up_to_height: usize) -> u64 {
  let mut total = 0u64;
  for height in 0..up_to_height {
    total += get_block_reward(height);
  }
  total
}

pub const fn calculate_dev_fee(block_reward: u64) -> u64 {
  (block_reward as f64 * DEV_FEE) as u64
}
