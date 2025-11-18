use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::Path;
use bincode::{Decode, Encode, config};
use num_bigint::BigUint;
use thiserror::Error;
use std::io;

use crate::core::block::Block;
use crate::core::crypto::Hash;
use crate::core::difficulty::{DifficultyManager, calculate_block_difficulty};
use crate::core::economics::{DEV_WALLET, calculate_dev_fee, get_block_reward};
use crate::core::transaction::TransactionId;
use crate::core::utxo::{TransactionError, UTXOs};

#[derive(Error, Debug)]
pub enum BlockchainError {
  #[error("IO error: {0}")]
  Io(#[from] io::Error),

  #[error("Bincode decode error: {0}")]
  BincodeDecode(#[from] Box<bincode::error::DecodeError>),

  #[error("Bincode encode error: {0}")]
  BincodeEncode(#[from] bincode::error::EncodeError),

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

  #[error("Double spend detected in transaction input: {0:?}")]
  DoubleSpend((TransactionId, usize)),

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
  NoBlocksToPop
}

impl From<TransactionError> for BlockchainError {
  fn from(err: TransactionError) -> Self {
    BlockchainError::InvalidTransaction(err.to_string())
  }
}

#[derive(Encode, Decode, Debug, Clone)]
struct Cache {
  difficulty_manager: DifficultyManager,
  height: usize,
  block_lookup: HashMap<Hash, usize>,
  utxos: UTXOs
}

#[derive(Debug)]
pub struct Blockchain {
  blockchain_path: String,
  height: usize,
  block_lookup: HashMap<Hash, usize>,
  utxos: UTXOs,
  difficulty_manager: DifficultyManager
}

impl Blockchain {
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
          difficulty_manager: cache.difficulty_manager
        }
      },
      Err(_) => {
        return Blockchain {
          blockchain_path,
          height: 0,
          block_lookup: HashMap::new(),
          utxos: UTXOs::new(),
          difficulty_manager: DifficultyManager::new(chrono::Utc::now().timestamp() as u64)
        }
      }
    }
  }

  fn load_cache(blockchain_path: &str) -> Result<Cache, BlockchainError> {
    let mut file = File::open(format!("{}cache.dat", blockchain_path))?;
    Ok(bincode::decode_from_std_read(&mut file, config::standard())
      .map_err(|e| BlockchainError::BincodeDecode(Box::new(e)))?)
  }

  fn save_cache(&self) -> Result<(), BlockchainError> {
    let mut file = File::create(format!("{}cache.dat", self.blockchain_path))?;
    let cache = Cache {
      difficulty_manager: self.difficulty_manager,
      height: self.height,
      block_lookup: self.block_lookup.clone(),
      utxos: self.utxos.clone()
    };

    bincode::encode_into_std_write(cache, &mut file, config::standard())?;
    Ok(())
  }

  fn blocks_dir(&self) -> String {
    format!("{}blocks/", &self.blockchain_path)
  }
  fn block_path_by_height(&self, height: usize) -> String {
    format!("{}{}.dat", self.blocks_dir(), height)
  }
  fn block_path_by_hash(&self, hash: &Hash) -> String {
    format!("{}{}.dat", self.blocks_dir(), self.block_lookup[hash])
  }

  pub fn add_block(&mut self, new_block: Block) -> Result<(), BlockchainError> {
    let block_hash = new_block.hash.ok_or(BlockchainError::MissingHash)?;

    if !block_hash.compare_with_data(&new_block.get_hashing_buf()?) {
      return Err(BlockchainError::HashMismatch {
        expected: Hash::new(&new_block.get_hashing_buf()?).dump_base36(),
        actual: block_hash.dump_base36(),
      });
    }

    if new_block.timestamp > chrono::Utc::now().timestamp() as u64 {
      return Err(BlockchainError::FutureTimestamp(new_block.timestamp));
    }

    if new_block.block_pow_difficulty != self.difficulty_manager.block_difficulty
      || new_block.tx_pow_difficulty != self.difficulty_manager.tx_difficulty
    {
      return Err(BlockchainError::InvalidDifficulty);
    }

    if BigUint::from_bytes_be(&*block_hash) > BigUint::from_bytes_be(&calculate_block_difficulty(&self.difficulty_manager.block_difficulty, new_block.transactions.len())) {
      return Err(BlockchainError::InvalidDifficulty);
    }

    let mut used_inputs: HashSet<(TransactionId, usize)> = HashSet::new();

    let mut seen_reward_transaction = false;
    for transaction in &new_block.transactions {
      if !transaction.inputs.is_empty() {
        self.utxos
          .validate_transaction(transaction, &BigUint::from_bytes_be(&self.difficulty_manager.tx_difficulty))?;

        for input in &transaction.inputs {
          let key = (input.transaction_id.clone(), input.output_index);
          if !used_inputs.insert(key.clone()) {
            return Err(BlockchainError::DoubleSpend(key));
          }
        }
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

        if transaction.outputs.iter().fold(0, |acc, output| acc + output.amount) != get_block_reward(self.height) {
          return Err(BlockchainError::InvalidRewardTransactionAmount);
        }

        let mut has_dev_fee = false;
        for output in &transaction.outputs {
          if output.receiver == DEV_WALLET && output.amount == calculate_dev_fee(get_block_reward(self.height)) {
            has_dev_fee = true;
            break;
          }
        }

        if !has_dev_fee {
          return Err(BlockchainError::NoDevFee);
        }
      }
    }

    for transaction in &new_block.transactions {
      self.utxos.execute_transaction(transaction);
    }

    self.difficulty_manager.update_difficulty(&new_block);
    self.block_lookup.insert(block_hash, self.height);

    let mut file = File::create(self.block_path_by_height(self.height))?;
    bincode::encode_into_std_write(new_block, &mut file, config::standard())?;

    self.height += 1;
    self.save_cache()?;

    Ok(())
  }

  pub fn pop_block(&mut self) -> Result<(), BlockchainError> {
    if self.height == 0 {
      return Err(BlockchainError::NoBlocksToPop);
    }

    let recalled_block = self.get_block_by_height(self.height - 1).ok_or(BlockchainError::MissingHash)?;
    // Rollback UTXO
    for transaction in &recalled_block.transactions {
      self.utxos.recall_transaction(transaction);
    }

    // Remove the block file
    fs::remove_file(self.block_path_by_height(self.height - 1))?;

    // Decrement height
    self.height -= 1;

    // Update difficulty manager
    self.difficulty_manager.block_difficulty = recalled_block.block_pow_difficulty;
    self.difficulty_manager.tx_difficulty = recalled_block.tx_pow_difficulty;
    if self.height > 0 && let Some(last_block) = self.get_block_by_height(self.height - 1) {
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
      None => None
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
}