use sled::Db;

use crate::{
    core::{
        block::Block,
        block_store::TransactionAndInfo,
        transaction::{TransactionId, TransactionOutput},
    },
    crypto::keys::Public,
    light_node::transaction_store::TransactionStore,
};

pub struct LightNodeUTXOs {
    /// Store available UTXOs for a set of addresses, where each available utxo list is stored indexed by the block they were created in.
    /// Key: address (32 bytes) ++ block height (8 bytes BE), Value: bincode Vec<(TransactionId, TransactionOutput, usize)>
    utxos: sled::Tree,
    /// Tracks which addresses we are scanning. Key: address (32 bytes), Value: empty
    watched: sled::Tree,
    /// Spend log for reorg support.
    /// Key: block height (8 bytes BE), Value: bincode Vec<(Vec<u8>, Vec<u8>)>
    /// Each entry is (utxo_key, encoded_outputs) — the exact rows removed/modified during scan_block at that height.
    spends: sled::Tree,
}

impl LightNodeUTXOs {
    pub fn new(db: &Db) -> sled::Result<Self> {
        Ok(Self {
            utxos: db.open_tree("utxos")?,
            watched: db.open_tree("watched")?,
            spends: db.open_tree("spends")?,
        })
    }

    fn utxo_key(address: &Public, height: usize) -> Vec<u8> {
        let mut key = address.dump_buf().to_vec();
        key.extend_from_slice(&height.to_be_bytes());
        key
    }

    fn encode<T: bincode::Encode>(value: &T) -> sled::Result<Vec<u8>> {
        bincode::encode_to_vec(value, bincode::config::standard())
            .map_err(|e| sled::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    }

    fn decode<T: bincode::Decode<()>>(bytes: &[u8]) -> sled::Result<T> {
        let (value, _) =
            bincode::decode_from_slice(bytes, bincode::config::standard()).map_err(|e| {
                sled::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?;
        Ok(value)
    }

    /// Returns none if address is not scanned
    pub fn get_utxos(
        &self,
        address: Public,
    ) -> sled::Result<Option<Vec<(TransactionId, TransactionOutput, usize)>>> {
        if !self.watched.contains_key(address.dump_buf())? {
            return Ok(None);
        }

        let prefix = address.dump_buf();
        let utxos = self
            .utxos
            .scan_prefix(prefix)
            .map(|res| {
                let (_, v) = res?;
                let outputs: Vec<(TransactionId, TransactionOutput, usize)> = Self::decode(&v)?;
                Ok(outputs)
            })
            .collect::<sled::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();

        Ok(Some(utxos))
    }

    pub fn calculate_confirmed_balance(&self, address: Public) -> u64 {
        let utxos = self.get_utxos(address).unwrap();
        if let Some(utxos) = utxos {
            utxos
                .iter()
                .fold(0u64, |acc, (_, utxo, _)| acc + utxo.amount)
        } else {
            0u64
        }
    }

    /// Start listening for transactions to the specified address
    pub fn add_address(&self, address: Public) -> sled::Result<()> {
        self.watched.insert(address.dump_buf(), &[])?;
        Ok(())
    }

    /// Stop listening for transactions to the specified address, and delete all stored utxos
    pub fn delete_address(&self, address: Public, tx_store: &TransactionStore) -> sled::Result<()> {
        self.watched.remove(address.dump_buf())?;

        // Remove all utxos for this address
        let keys: Vec<_> = self
            .utxos
            .scan_prefix(address.dump_buf())
            .map(|res| res.map(|(k, _)| k))
            .collect::<sled::Result<_>>()?;

        for key in keys {
            self.utxos.remove(key)?;
        }

        for tx in tx_store.iter_transactions() {
            let mut watched = self.get_watched();
            if !watched.any(|w| tx.transaction.contains_address(w)) {
                tx_store.remove_transaction(tx.transaction.transaction_id.unwrap());
            }
        }

        Ok(())
    }

    /// Get all watched addresses
    pub fn get_watched(&self) -> impl Iterator<Item = Public> + '_ {
        self.watched
            .iter()
            .map(|k| Public::new_from_buf(k.unwrap().0.as_ref().try_into().unwrap()))
    }

    /// Scan block for addresses we are listening for and add utxos if not yet added.
    /// WARNING: Block must be complete
    pub fn scan_block(
        &self,
        block: &Block,
        height: usize,
        tx_store: &TransactionStore,
    ) -> sled::Result<()> {
        let height_key = height.to_be_bytes();

        for res in self.watched.iter() {
            let (k, _) = res?;
            let address = Public::new_from_buf(k.as_ref().try_into().unwrap());

            let key = Self::utxo_key(&address, height);

            // We already scanned this block for this address
            if self.utxos.contains_key(&key)? {
                continue;
            }

            for transaction in &block.transactions {
                if transaction.contains_address(address) {
                    tx_store.put_transaction(TransactionAndInfo {
                        transaction: transaction.clone(),
                        at_height: height as u64,
                        in_block: block.meta.hash.unwrap(),
                    });
                }

                // Remove any utxos spent by this transaction, snapshotting rows for the spend log
                for input in &transaction.inputs {
                    if input.output_owner == address {
                        let spent_keys: Vec<_> = self
                            .utxos
                            .scan_prefix(address.dump_buf())
                            .map(|res| {
                                let (k, v) = res?;
                                let outputs: Vec<(TransactionId, TransactionOutput, usize)> =
                                    Self::decode(&v)?;
                                Ok((k, outputs))
                            })
                            .collect::<sled::Result<Vec<_>>>()?;

                        for (k, mut outputs) in spent_keys {
                            let before = outputs.len();
                            outputs.retain(|(tx_id, _, output_i)| {
                                !(*tx_id == input.transaction_id && *output_i == input.output_index)
                            });

                            if outputs.len() != before {
                                // Snapshot the old row into the spend log before mutating
                                if let Some(old_val) = self.utxos.get(&k)? {
                                    self.append_spend_log(
                                        &height_key,
                                        k.to_vec(),
                                        old_val.to_vec(),
                                    )?;
                                }

                                if outputs.is_empty() {
                                    self.utxos.remove(&k)?;
                                } else {
                                    let encoded = Self::encode(&outputs)?;
                                    self.utxos.insert(&k, encoded)?;
                                }
                            }
                        }
                    }
                }

                if transaction.contains_address(address) {
                    let mut new_outputs = vec![];
                    for (output_i, output) in transaction.outputs.iter().enumerate() {
                        if output.receiver == address {
                            new_outputs.push((
                                transaction.transaction_id.unwrap(),
                                *output,
                                output_i,
                            ));
                        }
                    }
                    let encoded = Self::encode(&new_outputs)?;
                    self.utxos.insert(&key, encoded)?;
                }
            }
        }

        Ok(())
    }

    /// Append a (utxo_key, old_encoded_value) pair to the spend log for the given height.
    /// Called before any row is removed or overwritten in scan_block.
    fn append_spend_log(
        &self,
        height_key: &[u8],
        utxo_key: Vec<u8>,
        old_value: Vec<u8>,
    ) -> sled::Result<()> {
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> =
            if let Some(existing) = self.spends.get(height_key)? {
                Self::decode(&existing)?
            } else {
                vec![]
            };

        entries.push((utxo_key, old_value));
        let encoded = Self::encode(&entries)?;
        self.spends.insert(height_key, encoded)?;
        Ok(())
    }

    /// Revert the last scanned block at the given height.
    ///
    /// This does two things:
    ///   1. Restores every UTXO row that was removed or partially spent during scan_block
    ///      at this height, using the spend log snapshot.
    ///   2. Deletes every UTXO row that was *created* at this height (i.e. new receives),
    ///      identified by the height suffix in the key.
    ///
    /// After this call the UTXO set reflects the state it was in before scan_block(height)
    /// was called. The spend log entry for this height is also cleaned up.
    pub fn pop_block(&self, height: usize) -> sled::Result<()> {
        let height_key = height.to_be_bytes();

        // Step 1: restore all rows that were removed or overwritten during this block
        if let Some(raw) = self.spends.get(&height_key)? {
            let entries: Vec<(Vec<u8>, Vec<u8>)> = Self::decode(&raw)?;
            for (k, v) in entries {
                self.utxos.insert(k, v)?;
            }
            self.spends.remove(&height_key)?;
        }

        // Step 2: remove all UTXO rows that were received in this block.
        // Keys are addr(32) ++ height(8), so we match on the last 8 bytes.
        let keys_to_remove: Vec<_> = self
            .utxos
            .iter()
            .filter_map(|res| res.ok().map(|(k, _)| k))
            .filter(|k| k.len() >= 8 && k[k.len() - 8..] == height_key)
            .collect();

        for k in keys_to_remove {
            self.utxos.remove(k)?;
        }

        Ok(())
    }
}
