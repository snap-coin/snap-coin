use bincode::{Decode, Encode};
use chrono::Utc;
use num_bigint::BigUint;
use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
};
use thiserror::Error;

use crate::{core::transaction::{self, Transaction, TransactionId, TransactionOutput}, crypto::keys::Public};

#[derive(Error, Debug)]
pub enum TransactionError {
    #[error("Transaction timestamp is in the future: {0}")]
    FutureTimestamp(u64),

    #[error("Transaction missing ID")]
    MissingId,

    #[error("Transaction hash is invalid: {0}")]
    InvalidHash(String),

    #[error("Transaction hash does not meet required difficulty: {0}")]
    InsufficientDifficulty(String),

    #[error("Transaction has no inputs")]
    NoInputs,

    #[error("Transaction input not found in UTXOs: {0}")]
    InputNotFound(String),

    #[error("Transaction input index invalid for transaction {tx_id}: {input_tx_id}")]
    InvalidInputIndex { tx_id: String, input_tx_id: String },

    #[error("Referenced transaction input is already spent")]
    SpentInputIndex,

    #[error("Transaction input signature is invalid for transaction {0}")]
    InvalidSignature(String),

    #[error("Double spending detected in the same transaction {0}")]
    DoubleSpend(String),

    #[error("Transaction output amount cannot be zero for transaction {0}")]
    ZeroOutput(String),

    #[error("Transaction inputs and outputs don't sum up to same amount for transaction {0}")]
    SumMismatch(String),

    #[error("Transaction has too many inputs or outputs")]
    TooManyIO,
}

/// This represents a singular transaction undo, or a whole block, essentially we need to extend each of these lists to combine with other utxo diffs (prob. from other tx's)
#[derive(Encode, Decode, Debug, Clone)]
pub struct UTXODiff {
    spent: Vec<(TransactionId, usize, TransactionOutput)>,
    created: Vec<(TransactionId, usize, TransactionOutput)>,
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

#[derive(Encode, Decode, Clone, Debug)]
pub struct UTXOs {
    /// Map of transaction (id's) and its outputs
    pub utxos: HashMap<TransactionId, Vec<Option<TransactionOutput>>>,
}

impl UTXOs {
    /// Create a new empty UTXO set
    pub fn new() -> Self {
        UTXOs {
            utxos: HashMap::new(),
        }
    }

    /// Validate a certain transaction in the context of these UTXOs
    /// WARNING: Timestamp validation is not checked here!
    pub fn validate_transaction(
        &self,
        transaction: &Transaction,
        tx_hashing_difficulty: &BigUint,
    ) -> Result<(), TransactionError> {
        let tx_id = transaction
            .transaction_id
            .ok_or(TransactionError::MissingId)?;
        let input_signing_buf = transaction
            .get_input_signing_buf()
            .map_err(|_| TransactionError::InvalidHash(tx_id.dump_base36()))?;
        let transaction_hashing_buf = transaction
            .get_tx_hashing_buf()
            .map_err(|_| TransactionError::InvalidHash(tx_id.dump_base36()))?;

        if transaction.timestamp > Utc::now().timestamp() as u64 {
            return Err(TransactionError::FutureTimestamp(transaction.timestamp));
        }

        if !tx_id.compare_with_data(&transaction_hashing_buf) {
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

        if transaction.inputs.len() > transaction::MAX_TRANSACTION_IO
            || transaction.outputs.len() > transaction::MAX_TRANSACTION_IO
        {
            return Err(TransactionError::TooManyIO);
        }

        let mut used_utxos: HashSet<(TransactionId, usize)> = HashSet::new();
        let mut input_sum: u64 = 0;
        let mut output_sum: u64 = 0;

        for input in &transaction.inputs {
            let prev_outputs = self
                .utxos
                .get(&input.transaction_id)
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

            if input.signature.is_none() || input.signature.unwrap().validate_with_public(&output.unwrap().receiver, &input_signing_buf).map_or(true, |valid| !valid) {
                return Err(TransactionError::InvalidSignature(tx_id.dump_base36()));
            }

            let utxo_key = (input.transaction_id, input.output_index);
            if used_utxos.contains(&utxo_key) {
                return Err(TransactionError::DoubleSpend(tx_id.dump_base36()));
            }
            used_utxos.insert(utxo_key);

            input_sum += output.unwrap().amount;
        }

        for output in &transaction.outputs {
            if output.amount == 0 {
                return Err(TransactionError::ZeroOutput(tx_id.dump_base36()));
            }
            output_sum += output.amount;
        }

        if input_sum != output_sum {
            return Err(TransactionError::SumMismatch(tx_id.dump_base36()));
        }

        Ok(())
    }

    /// Execute a already valid transaction.
    /// Returns a UTXODiff object that can be used to undo these changes
    /// WARNING: Transaction validity must be checked before calling this function
    pub fn execute_transaction(&mut self, transaction: &Transaction) -> UTXODiff {
        // Keep track of created and removed utxos
        let mut spent_utxos: Vec<(TransactionId, usize, TransactionOutput)> = Vec::new();
        let mut created_utxos: Vec<(TransactionId, usize, TransactionOutput)> = Vec::new();
        for input in &transaction.inputs {
            if let Some(outputs) = self.utxos.get_mut(&input.transaction_id) {
                let spent = outputs[input.output_index].clone().unwrap();
                outputs[input.output_index] = None;

                if outputs.iter().all(|o| o.is_none()) {
                    self.utxos.remove(&input.transaction_id);
                }
                spent_utxos.push((
                    input.transaction_id,
                    input.output_index,
                    spent,
                ));
            }
        }

        self.utxos.insert(
            transaction.transaction_id.unwrap(),
            transaction.outputs.iter().map(|o| Some(*o)).collect(),
        );

        for (output_index, output) in transaction.outputs.iter().enumerate() {
            created_utxos.push((transaction.transaction_id.unwrap(), output_index, *output));
        }

        // println!("Executing transaction! added {:#?}", created_utxos);
        UTXODiff {
            spent: spent_utxos,
            created: created_utxos,
        }
    }

    /// Undo a certain transactions with its UTXODiff
    pub fn recall_block_utxos(&mut self, diffs: UTXODiff) {
        for spent in diffs.spent {
            if self.utxos.get(&spent.0).is_some() {
                self.utxos.get_mut(&spent.0).unwrap()[spent.1] = Some(spent.2);
            } else {
                let mut outputs = vec![None; spent.1 + 1];
                outputs[spent.1] = Some(spent.2);
                self.utxos.insert(spent.0, outputs);
            }
        }

        for created in diffs.created {
            if let Some(vec) = self.utxos.get_mut(&created.0) {
                vec[created.1] = None;
                if vec.iter().all(|x| x.is_none()) {
                    self.utxos.remove(&created.0);
                }
            }
        }

    }

    pub fn calculate_confirmed_balance(&self, address: Public) -> u64 {
        let mut balance = 0u64;

        for transaction_out in self.utxos.values().flat_map(|utxos| utxos) {
            if let Some(transaction_out) = transaction_out {
                if transaction_out.receiver == address {
                    balance += transaction_out.amount;
                }
            }
        }

        balance
    }

    pub fn get_utxos(&self, address: Public) -> Vec<TransactionId> {
        let mut utxos: Vec<TransactionId> = vec![];

        for (transaction_id, outputs) in &self.utxos {
            if outputs.iter().any(|output| output.is_some_and(|output| output.receiver == address)) {
                utxos.push(*transaction_id);
            }
        }

        utxos
    }
}
