use std::{collections::{HashMap, HashSet}, ops::Deref};
use bincode::{Decode, Encode};
use chrono::Utc;
use num_bigint::BigUint;
use thiserror::Error;

use crate::core::transaction::{self, Transaction, TransactionId, TransactionOutput};

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

  #[error("Transaction input's and output's amounts or funders / receivers do not match up")]
  InputOutputMismatch,

  #[error("Transaction has too many inputs or outputs")]
  TooManyIO
}

#[derive(Encode, Decode, Clone, Debug)]
pub struct UTXOs {
  pub utxos: HashMap<TransactionId, Vec<Option<TransactionOutput>>>
}

impl UTXOs {
  pub fn new() -> Self {
    UTXOs { utxos: HashMap::new() }
  }

  pub fn validate_transaction(
    &self,
    transaction: &Transaction,
    tx_hashing_difficulty: &BigUint
  ) -> Result<(), TransactionError> {

    let tx_id = transaction.transaction_id.ok_or(TransactionError::MissingId)?;
    let input_signing_buf = transaction.get_input_signing_buf()
      .map_err(|_| TransactionError::InvalidHash(tx_id.dump_base36()))?;
    let transaction_hashing_buf = transaction.get_tx_hashing_buf()
      .map_err(|_| TransactionError::InvalidHash(tx_id.dump_base36()))?;

    if transaction.timestamp > Utc::now().timestamp() as u64 {
      return Err(TransactionError::FutureTimestamp(transaction.timestamp));
    }

    if !tx_id.compare_with_data(&transaction_hashing_buf) {
      return Err(TransactionError::InvalidHash(tx_id.dump_base36()));
    }

    if BigUint::from_bytes_be(tx_id.deref()) > *tx_hashing_difficulty {
      return Err(TransactionError::InsufficientDifficulty(tx_id.dump_base36()));
    }

    if transaction.inputs.is_empty() {
      return Err(TransactionError::NoInputs);
    }

    if transaction.inputs.len() > transaction::MAX_TRANSACTION_IO || transaction.outputs.len() > transaction::MAX_TRANSACTION_IO {
      return Err(TransactionError::TooManyIO);
    }

    let mut used_utxos: HashSet<(TransactionId, usize)> = HashSet::new();
    let mut input_sum: u64 = 0;
    let mut output_sum: u64 = 0;

    for input in &transaction.inputs {
      let prev_outputs = self.utxos.get(&input.transaction_id)
        .ok_or(TransactionError::InputNotFound(tx_id.dump_base36()))?;

      let output = prev_outputs.get(input.output_index)
        .ok_or(TransactionError::InvalidInputIndex { 
          tx_id: tx_id.dump_base36(), 
          input_tx_id: input.transaction_id.dump_base36() 
        })?;

      if output.is_none() {
        return Err(TransactionError::SpentInputIndex);
      }

      if input.amount != output.unwrap().amount || input.funder != output.unwrap().receiver {
        return Err(TransactionError::InputOutputMismatch);
      }

      if input.signature.as_ref().map_or(true, |sig| sig.validate_with_public(&output.unwrap().receiver, &input_signing_buf).is_err()) {
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

  pub fn execute_transaction(&mut self, transaction: &Transaction) {
    for input in &transaction.inputs {
      if let Some(outputs) = self.utxos.get_mut(&input.transaction_id) {
        outputs[input.output_index] = None;

        if outputs.iter().all(|o| o.is_none()) {
          self.utxos.remove(&input.transaction_id);
        }
      }
    }

    self.utxos.insert(transaction.transaction_id.unwrap(), transaction.outputs.iter().map(|o| Some(*o)).collect());
  }

  pub fn recall_transaction(&mut self, transaction: &Transaction) {
    for input in &transaction.inputs {
      match self.utxos.get_mut(&input.transaction_id) {
        Some(outputs ) => {
          outputs[input.output_index] = Some(TransactionOutput { amount: input.amount, receiver: input.funder })
        },
        None => {
          let mut outputs: Vec<Option<TransactionOutput>> = vec![None; input.output_index + 1];
          outputs[input.output_index] = Some(TransactionOutput { amount: input.amount, receiver: input.funder });

          self.utxos.insert(input.transaction_id, outputs);
        }
      }
    }

    self.utxos.remove(&transaction.transaction_id.unwrap());
  }
}
