use bincode::{Decode, Encode};
use num_bigint::BigUint;
use std::collections::HashSet;
use std::ops::Deref;

use crate::{
    core::transaction::{self, Transaction, TransactionError, TransactionId, TransactionOutput},
    crypto::keys::Public,
};

#[derive(Encode, Decode, Debug, Clone)]
pub struct UTXODiff {
    pub spent: Vec<(TransactionId, usize, TransactionOutput)>,
    pub created: Vec<(TransactionId, usize, TransactionOutput)>,
}

impl UTXODiff {
    pub fn new_empty() -> Self {
        Self {
            spent: Vec::new(),
            created: Vec::new(),
        }
    }

    pub fn extend(&mut self, other: &mut Self) {
        self.spent.append(&mut other.spent);
        self.created.append(&mut other.created);
    }
}

#[derive(Clone, Debug)]
pub struct UTXOs {
    pub db: sled::Db,
}

impl UTXOs {
    /// Open or create a disk-backed UTXO store
    pub fn new(utxos_path: impl AsRef<std::path::Path>) -> Self {
        let db = sled::open(utxos_path).expect("Failed to open UTXO database");
        Self { db }
    }

    /// Get outputs of a transaction
    fn get_tx_outputs(&self, txid: &TransactionId) -> Option<Vec<Option<TransactionOutput>>> {
        let key = txid.dump_base36();
        let value = self.db.get(key).ok().flatten()?;
        let (outputs, _) = bincode::decode_from_slice(&value, bincode::config::standard()).ok()?;
        Some(outputs)
    }

    /// Persist outputs of a transaction
    fn set_tx_outputs(
        &self,
        txid: &TransactionId,
        outputs: &[Option<TransactionOutput>],
    ) -> Result<(), TransactionError> {
        let key = txid.dump_base36();
        let buffer = bincode::encode_to_vec(outputs, bincode::config::standard())
            .map_err(|_| TransactionError::Other("Failed to encode UTXOs".to_string()))?;
        self.db
            .insert(key, buffer)
            .map_err(|e| TransactionError::Other(e.to_string()))?;

        Ok(())
    }

    /// Delete a transaction from the store
    fn delete_tx(&self, txid: &TransactionId) -> Result<(), TransactionError> {
        self.db
            .remove(txid.dump_base36())
            .map_err(|e| TransactionError::Other(e.to_string()))?;

        Ok(())
    }

    /// Validate a transaction in the context of these UTXOs
    pub fn validate_transaction(
        &self,
        transaction: &Transaction,
        tx_hashing_difficulty: &BigUint,
        is_ibd: bool,
    ) -> Result<(), TransactionError> {
        let tx_id = transaction
            .transaction_id
            .ok_or(TransactionError::MissingId)?;
        let input_signing_buf = transaction.get_input_signing_buf()?;
        let transaction_hashing_buf = transaction.get_tx_hashing_buf()?;

        if !is_ibd && !tx_id.compare_with_data(&transaction_hashing_buf) {
            return Err(TransactionError::InvalidHash(tx_id.dump_base36()));
        }

        if BigUint::from_bytes_be(tx_id.deref()) > *tx_hashing_difficulty {
            return Err(TransactionError::InsufficientDifficulty(
                tx_id.dump_base36(),
            ));
        }

        if transaction.inputs.is_empty() {
            return Err(TransactionError::NoInputs);
        }

        if transaction.inputs.len() + transaction.outputs.len() > transaction::MAX_TRANSACTION_IO {
            return Err(TransactionError::TooMuchIO);
        }

        transaction.check_completeness()?; // Just in case

        let mut used_utxos: HashSet<(TransactionId, usize)> = HashSet::new();
        let mut input_sum: u64 = 0;
        let mut output_sum: u64 = 0;

        for input in &transaction.inputs {
            let prev_outputs = self
                .get_tx_outputs(&input.transaction_id)
                .ok_or(TransactionError::InputNotFound(tx_id.dump_base36()))?;

            let output = prev_outputs.get(input.output_index).ok_or(
                TransactionError::InvalidInputIndex {
                    tx_id: tx_id.dump_base36(),
                    input_tx_id: input.transaction_id.dump_base36(),
                },
            )?;

            if output.is_none() {
                return Err(TransactionError::SpentInputIndex);
            }

            if input.signature.is_none()
                || input
                    .signature
                    .unwrap()
                    .validate_with_public(&output.unwrap().receiver, &input_signing_buf)
                    .map_or(true, |valid| !valid)
            {
                return Err(TransactionError::InvalidSignature(tx_id.dump_base36()));
            }

            if output.unwrap().receiver != input.output_owner {
                return Err(TransactionError::IncorrectOutputOwner(tx_id.dump_base36()));
            }

            let utxo_key = (input.transaction_id, input.output_index);
            if used_utxos.contains(&utxo_key) {
                return Err(TransactionError::DoubleSpend(tx_id.dump_base36()));
            }
            used_utxos.insert(utxo_key);

            input_sum = input_sum
                .checked_add(output.unwrap().amount)
                .ok_or(TransactionError::OverflowError)?;
        }

        for output in &transaction.outputs {
            if output.amount == 0 {
                return Err(TransactionError::ZeroOutput(tx_id.dump_base36()));
            }
            output_sum = output_sum
                .checked_add(output.amount)
                .ok_or(TransactionError::OverflowError)?;
        }

        if input_sum != output_sum {
            return Err(TransactionError::SumMismatch(tx_id.dump_base36()));
        }

        Ok(())
    }

    /// Execute a valid transaction
    pub fn execute_transaction(
        &self,
        transaction: &Transaction,
    ) -> Result<UTXODiff, TransactionError> {
        let tx_id = transaction.transaction_id.unwrap();

        let mut spent_utxos = Vec::new();
        let mut created_utxos = Vec::new();

        // Spend inputs
        for input in &transaction.inputs {
            let mut prev_outputs = self
                .get_tx_outputs(&input.transaction_id)
                .ok_or(TransactionError::InputNotFound(tx_id.dump_base36()))?;

            let spent = prev_outputs[input.output_index].take().unwrap();
            spent_utxos.push((input.transaction_id, input.output_index, spent));

            if prev_outputs.iter().all(|o| o.is_none()) {
                self.delete_tx(&input.transaction_id)?;
            } else {
                self.set_tx_outputs(&input.transaction_id, &prev_outputs)?;
            }
        }

        // Create outputs
        let outputs: Vec<Option<TransactionOutput>> =
            transaction.outputs.iter().map(|o| Some(*o)).collect();
        self.set_tx_outputs(&tx_id, &outputs)?;
        for (index, output) in transaction.outputs.iter().enumerate() {
            created_utxos.push((tx_id, index, *output));
        }

        self.db
            .flush()
            .map_err(|e| TransactionError::Other(e.to_string()))?;

        Ok(UTXODiff {
            spent: spent_utxos,
            created: created_utxos,
        })
    }

    /// Undo a transaction using its UTXODiff
    pub fn recall_block_utxos(&self, diffs: &UTXODiff) -> Result<(), TransactionError> {
        // Restore spent outputs
        for (txid, idx, output) in &diffs.spent {
            let mut outputs = self
                .get_tx_outputs(txid)
                .unwrap_or_else(|| vec![None; *idx + 1]);
            if outputs.len() <= *idx {
                outputs.resize(idx + 1, None);
            }
            outputs[*idx] = Some(*output);
            self.set_tx_outputs(txid, &outputs)?;
        }

        // Remove created outputs
        for (txid, idx, _) in &diffs.created {
            let mut outputs = self
                .get_tx_outputs(txid)
                .ok_or(TransactionError::Other("Missing created UTXO".to_string()))?;
            outputs[*idx] = None;
            if outputs.iter().all(|o| o.is_none()) {
                self.delete_tx(txid)?;
            } else {
                self.set_tx_outputs(txid, &outputs)?;
            }
        }

        Ok(())
    }

    /// Calculate balance of an address
    pub fn calculate_confirmed_balance(&self, address: Public) -> u64 {
        self.db
            .iter()
            .values()
            .filter_map(|res| res.ok())
            .flat_map(|v: sled::IVec| {
                bincode::decode_from_slice::<Vec<Option<TransactionOutput>>, _>(
                    &v,
                    bincode::config::standard(),
                )
                .ok()
                .map(|(o, _)| o)
                .unwrap_or_default()
            })
            .filter_map(|out| out)
            .filter(|out| out.receiver == address)
            .map(|out| out.amount)
            .sum()
    }

    /// Get all transaction IDs with unspent outputs for an address
    pub fn get_utxos(&self, address: Public) -> Vec<TransactionId> {
        self.db
            .iter()
            .filter_map(|res| res.ok())
            .filter_map(|(k, v)| {
                let txid = TransactionId::new_from_base36(&String::from_utf8(k.to_vec()).ok()?)?;
                let outputs: Vec<Option<TransactionOutput>> =
                    bincode::decode_from_slice(&v, bincode::config::standard())
                        .ok()?
                        .0;
                if outputs
                    .iter()
                    .any(|o| o.is_some_and(|o| o.receiver == address))
                {
                    Some(txid)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns all unspent outputs in the database.
    pub fn get_all_utxos(&self) -> Vec<(TransactionId, TransactionOutput, usize)> {
        let mut all_utxos = Vec::new();

        for item in self.db.iter() {
            if let Ok((txid_bytes, value)) = item {
                // Convert bytes to transaction ID
                if let Ok(txid_str) = String::from_utf8(txid_bytes.to_vec()) {
                    if let Some(txid) = TransactionId::new_from_base36(&txid_str) {
                        // Decode outputs
                        if let Ok((outputs, _)) =
                            bincode::decode_from_slice::<Vec<Option<TransactionOutput>>, _>(
                                &value,
                                bincode::config::standard(),
                            )
                        {
                            for (index, output_opt) in outputs.into_iter().enumerate() {
                                if let Some(output) = output_opt {
                                    all_utxos.push((txid, output, index));
                                }
                            }
                        }
                    }
                }
            }
        }

        all_utxos
    }
}
