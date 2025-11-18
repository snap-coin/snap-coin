use rand::Rng;
use bincode::{Decode, Encode, error::EncodeError};
use num_bigint::BigUint;

use crate::core::{crypto::Hash, difficulty::calculate_block_difficulty, transaction::Transaction};

#[derive(Encode, Decode, Clone, Debug)]
pub struct Block {
  pub transactions: Vec<Transaction>,
  pub timestamp: u64,
  pub nonce: u64,
  pub block_pow_difficulty: [u8; 32],
  pub tx_pow_difficulty: [u8; 32],
  pub hash: Option<Hash>
}

impl Block {
  pub fn new_block_now(transactions: Vec<Transaction>, block_pow_difficulty: &[u8; 32], tx_pow_difficulty: &[u8; 32]) -> Self {
    Block {
      transactions,
      timestamp: chrono::Utc::now().timestamp() as u64,
      nonce: 0,
      block_pow_difficulty: *block_pow_difficulty,
      tx_pow_difficulty: *tx_pow_difficulty,
      hash: None,
    }
  }

  pub fn get_hashing_buf(&self) -> Result<Vec<u8>, EncodeError> {
    let mut hash_less_block = self.clone();
    hash_less_block.hash = None;
    bincode::encode_to_vec(hash_less_block, bincode::config::standard())
  }

  pub fn compute_pow(&mut self) -> Result<(), EncodeError> {
    let tx_difficulty_big_int = BigUint::from_bytes_be(&calculate_block_difficulty(&self.block_pow_difficulty, self.transactions.len()));
    let mut rng = rand::rng();
    loop {
      self.nonce = rng.random();
      let hashing_buf = self.get_hashing_buf()?;
      if BigUint::from_bytes_be(&*Hash::new(&hashing_buf)) <= tx_difficulty_big_int {
        self.hash = Some(Hash::new(&hashing_buf));
        return Ok(())
      }
    }
  }
}