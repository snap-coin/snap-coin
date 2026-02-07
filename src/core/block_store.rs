use std::{
    fs::{self, File},
    io::Write,
    sync::RwLock,
};

use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    core::{
        block::{Block, BlockError},
        transaction::{Transaction, TransactionId},
        utxo::UTXODiff,
    },
    crypto::Hash,
    economics::GENESIS_PREVIOUS_BLOCK_HASH,
};

#[derive(Error, Serialize, Deserialize, Debug, Clone, Encode, Decode)]
pub enum BlockStoreError {
    #[error("No blocks left to pop")]
    NoneToPop,

    #[error("Encoding failed")]
    Encode,

    #[error("block error: {0}")]
    BlockError(#[from] BlockError),

    #[error("IO error: {0}")]
    IO(String),
}

impl From<std::io::Error> for BlockStoreError {
    fn from(value: std::io::Error) -> Self {
        BlockStoreError::IO(value.to_string())
    }
}

#[derive(Encode, Decode, Clone, Debug, Serialize, Deserialize)]
pub struct TransactionAndInfo {
    pub transaction: Transaction,
    pub at_height: u64,
    pub in_block: Hash,
}

pub struct BlockIndex {
    pub db: sled::Db,
    pub by_hash: sled::Tree,
    pub by_height: sled::Tree,
}

impl BlockIndex {
    pub fn load(path: &str) -> Self {
        if !fs::exists(path).expect("Could not open block index DB") {
            fs::create_dir_all(path).expect("Could not open block index DB");
        }
        let db = sled::open(path).expect("Could not open block index DB");
        let by_hash = db
            .open_tree("by_hash")
            .expect("Could not open block index by hash tree");
        let by_height = db
            .open_tree("by_height")
            .expect("Could not open block index by height tree");
        Self {
            db,
            by_hash,
            by_height,
        }
    }
}

pub struct BlockStore {
    pub store_path: String,
    block_index: BlockIndex,
    height: RwLock<usize>,
    last_block: RwLock<Hash>,
}

impl BlockStore {
    pub fn new_empty(path: &str) -> Self {
        Self {
            store_path: path.to_owned(),
            block_index: BlockIndex::load(&format!("{}block-index", path)),
            height: RwLock::new(0usize),
            last_block: RwLock::new(GENESIS_PREVIOUS_BLOCK_HASH),
        }
    }

    pub fn load(path: &str, height: usize, last_block: Hash) -> Self {
        Self {
            store_path: path.to_owned(),
            block_index: BlockIndex::load(&format!("{}block-index", path)),
            height: RwLock::new(height),
            last_block: RwLock::new(last_block),
        }
    }

    /// Adds a block, writing it to disk, updating block lookup and current height
    /// WARNING: block MUST be valid beforehand!
    pub fn add_block(&self, block: Block, diffs: UTXODiff) -> Result<(), BlockStoreError> {
        block.check_completeness()?;

        let block_tmp = format!("{}.tmp", self.block_path_by_height(self.get_height()));
        let diffs_tmp = format!("{}.tmp", self.utxo_diffs_path_by_height(self.get_height()));

        // Serialize
        let block_buffer = bincode::encode_to_vec(block.clone(), bincode::config::standard())
            .map_err(|_| BlockStoreError::Encode)?;
        let diffs_buffer: Vec<u8> = bincode::encode_to_vec(diffs, bincode::config::standard())
            .map_err(|_| BlockStoreError::Encode)?;

        // Write temp block
        {
            let mut f = File::create(&block_tmp)?;
            f.write_all(&block_buffer)?;
            f.sync_all()?;
        }

        // Write temp diffs
        {
            let mut f = File::create(&diffs_tmp)?;
            f.write_all(&diffs_buffer)?;
            f.sync_all()?;
        }

        // Atomic rename
        fs::rename(&block_tmp, self.block_path_by_height(self.get_height()))?;
        fs::rename(
            &diffs_tmp,
            self.utxo_diffs_path_by_height(self.get_height()),
        )?;

        // Update block index
        // Unwraps are okay, we checked, block is complete
        self.block_index
            .by_hash
            .insert(
                block.meta.hash.unwrap().dump_buf(),
                &self.get_height().to_be_bytes(),
            )
            .unwrap();
        self.block_index
            .by_height
            .insert(
                &self.get_height().to_be_bytes(),
                &block.meta.hash.unwrap().dump_buf(),
            )
            .unwrap();

        // Flush database
        self.block_index.db.flush().unwrap();

        // Update block height and last block
        *self.height.write().unwrap() = self.get_height() + 1;
        *self.last_block.write().unwrap() = block.meta.hash.unwrap();
        Ok(())
    }

    pub fn pop_block(&self) -> Result<(), BlockStoreError> {
        if self.get_height() == 0 {
            return Err(BlockStoreError::NoneToPop);
        }
        let height = self.get_height() - 1;

        // Paths
        let block_path = self.block_path_by_height(height);
        let diffs_path = self.utxo_diffs_path_by_height(height);

        let block_del = format!("{}.delete", block_path);
        let diffs_del = format!("{}.delete", diffs_path);

        // Atomically rename files out of the active set
        fs::rename(&block_path, &block_del)?;
        fs::rename(&diffs_path, &diffs_del)?;

        // Remove block from index

        self.block_index
            .by_hash
            .remove(&self.get_last_block_hash().dump_buf())
            .unwrap();
        self.block_index
            .by_height
            .remove(&height.to_be_bytes())
            .unwrap();

        // Flush database
        self.block_index.db.flush().unwrap();

        // Update height first
        *self.height.write().unwrap() = height;

        // Update last_block correctly
        if height == 0 {
            *self.last_block.write().unwrap() = GENESIS_PREVIOUS_BLOCK_HASH;
        } else {
            let last_block_after_pop = self.get_last_block().ok_or(BlockError::IncompleteBlock)?;
            *self.last_block.write().unwrap() = last_block_after_pop.meta.hash.unwrap();
        }

        // Final delete (non-atomic, but safe now)
        fs::remove_file(block_del)?;
        fs::remove_file(diffs_del)?;

        Ok(())
    }

    /// Gets current block height (count of all blocks)
    pub fn get_height(&self) -> usize {
        *self.height.read().unwrap()
    }

    /// Gets last added block hash (GENESIS_PREVIOUS_BLOCK_HASH if height = 0)
    pub fn get_last_block_hash(&self) -> Hash {
        *self.last_block.read().unwrap()
    }

    /// Gets last added block
    pub fn get_last_block(&self) -> Option<Block> {
        self.get_block_by_height(self.get_height().saturating_sub(1))
    }

    /// Gets block referenced by it's height
    pub fn get_block_by_height(&self, height: usize) -> Option<Block> {
        if height > self.get_height() {
            return None;
        }

        let path = self.block_path_by_height(height);
        Self::load_block_from_path(&path).ok()
    }

    /// Gets block referenced by it's hash
    pub fn get_block_by_hash(&self, hash: Hash) -> Option<Block> {
        self.get_block_by_height(self.get_block_height_by_hash(hash)?)
    }

    /// Gets block hash referenced by it's height
    pub fn get_block_hash_by_height(&self, height: usize) -> Option<Hash> {
        if let Some(buf) = self
            .block_index
            .by_height
            .get(height.to_be_bytes())
            .expect("Could not read block index DB")
        {
            Some(Hash::new_from_buf(buf.as_ref().try_into().unwrap()))
        } else {
            None
        }
    }

    /// Gets block height referenced by it's hash
    pub fn get_block_height_by_hash(&self, hash: Hash) -> Option<usize> {
        if let Some(buf) = self
            .block_index
            .by_hash
            .get(hash.dump_buf())
            .expect("Could not read block index DB")
        {
            Some(usize::from_be_bytes(buf.as_ref().try_into().unwrap()))
        } else {
            None
        }
    }

    pub fn get_last_utxo_diffs(&self) -> Option<UTXODiff> {
        let path = self.utxo_diffs_path_by_height(self.get_height() - 1);
        Self::load_utxo_diffs_from_path(&path).ok()
    }

    /// Gets UTXO diffs by block height
    pub fn get_utxo_diffs_by_height(&self, height: usize) -> Option<UTXODiff> {
        if height == 0 || height > self.get_height() {
            return None;
        }

        let path = self.utxo_diffs_path_by_height(height);
        Self::load_utxo_diffs_from_path(&path).ok()
    }

    /// Gets UTXO diffs by block hash
    pub fn get_utxo_diffs_by_hash(&self, hash: Hash) -> Option<UTXODiff> {
        let height = self.get_block_height_by_hash(hash)?;
        self.get_utxo_diffs_by_height(height)
    }

    fn load_utxo_diffs_from_path(path: &str) -> Result<UTXODiff, BlockStoreError> {
        let data = fs::read(path).map_err(|e| BlockStoreError::IO(e.to_string()))?;
        let (diffs, _) = bincode::decode_from_slice(&data, bincode::config::standard())
            .map_err(|_| BlockStoreError::Encode)?;
        Ok(diffs)
    }

    fn load_block_from_path(path: &str) -> Result<Block, BlockStoreError> {
        let data = fs::read(path).map_err(|_| BlockStoreError::Encode)?;
        let (block, _) = bincode::decode_from_slice(&data, bincode::config::standard())
            .map_err(|_| BlockStoreError::Encode)?;
        Ok(block)
    }

    fn block_path_by_height(&self, height: usize) -> String {
        format!("{}{}.dat", self.store_path, height)
    }
    fn utxo_diffs_path_by_height(&self, height: usize) -> String {
        format!("{}utxo-diffs-{}.dat", self.store_path, height)
    }

    pub fn get_transaction(&self, tx_id: TransactionId) -> Option<Transaction> {
        // Iterate from newest to oldest for faster access if likely recent
        let height = self.get_height();
        for h in (0..height).rev() {
            if let Some(block) = self.get_block_by_height(h) {
                for tx in block.transactions {
                    if tx.transaction_id == Some(tx_id) {
                        return Some(tx);
                    }
                }
            }
        }
        None
    }

    pub fn get_transaction_and_info(&self, tx_id: TransactionId) -> Option<TransactionAndInfo> {
        // Iterate from newest to oldest for faster access if likely recent
        let height = self.get_height();
        for h in (0..height).rev() {
            if let Some(block) = self.get_block_by_height(h) {
                for tx in block.transactions {
                    if tx.transaction_id == Some(tx_id) {
                        return Some(TransactionAndInfo {
                            transaction: tx,
                            at_height: h as u64,
                            in_block: block.meta.hash.unwrap_or(Hash::new_from_buf([0u8; 32])),
                        });
                    }
                }
            }
        }
        None
    }

    pub fn iter_blocks(
        &self,
    ) -> impl DoubleEndedIterator<Item = Result<Block, BlockStoreError>> + '_ {
        let height = self.get_height();
        (0..height).map(move |h| {
            let path = self.block_path_by_height(h);
            Self::load_block_from_path(&path)
        })
    }
}
