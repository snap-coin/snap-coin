
use bincode::{Decode, Encode, error::EncodeError};
use num_bigint::BigUint;
use rand::Rng;

use crate::core::{crypto::{Hash, Signature}, keys::{Private, Public}};

pub type TransactionId = Hash;

pub const MAX_TRANSACTION_IO: usize = 500;

#[derive(Encode, Decode, Debug, Clone, Copy)]
pub struct TransactionInput {
  pub transaction_id: TransactionId,
  pub output_index: usize,
  pub amount: u64,
  pub funder: Public,
  pub signature: Option<Signature>,
}

#[derive(Encode, Decode, Debug, Clone, Copy)]
pub struct TransactionOutput {
  pub amount: u64,
  pub receiver: Public
}

#[derive(Encode, Decode, Debug, Clone)]
pub struct Transaction {
  pub inputs: Vec<TransactionInput>,
  pub outputs: Vec<TransactionOutput>,
  pub transaction_id: Option<TransactionId>,
  pub nonce: u64,
  pub timestamp: u64
}

impl Transaction {
  pub fn new_transaction_now(inputs: Vec<TransactionInput>, outputs: Vec<TransactionOutput>, signing_keys: &mut Vec<Private>) -> Result<Self, EncodeError> {
    let mut transaction = Self {
      inputs,
      outputs,
      transaction_id: None,
      nonce: 0,
      timestamp: chrono::Utc::now().timestamp() as u64,
    };
    let signing_buf = transaction.get_input_signing_buf()?;

    for (input, key) in transaction.inputs.iter_mut().zip(signing_keys.iter_mut()) {
      input.signature = Some(Signature::new_singature(key, &signing_buf));
    }

    Ok(transaction)
  }

  pub fn compute_pow(&mut self, tx_difficulty: &[u8; 32]) -> Result<(), EncodeError> {
    let tx_difficulty_big_int = BigUint::from_bytes_be(tx_difficulty);
    let mut rng = rand::rng();
    loop {
      self.nonce = rng.random();
      let hashing_buf = self.get_tx_hashing_buf()?;
      if BigUint::from_bytes_be(&*Hash::new(&hashing_buf)) <= tx_difficulty_big_int {
        self.transaction_id = Some(Hash::new(&hashing_buf));
        return Ok(())
      }
    }
  }

  pub fn get_input_signing_buf(&self) -> Result<Vec<u8>, EncodeError> {
    let mut signature_less_transaction = self.clone();

    signature_less_transaction.transaction_id = None;

    for input in &mut signature_less_transaction.inputs {
      input.signature = None; // remove all signatures for signing
    }

    Ok(bincode::encode_to_vec(signature_less_transaction, bincode::config::standard())?)
  }

  pub fn get_tx_hashing_buf(&self) -> Result<Vec<u8>, EncodeError> {
    let mut signature_less_transaction = self.clone();

    signature_less_transaction.transaction_id = None;

    Ok(bincode::encode_to_vec(signature_less_transaction, bincode::config::standard())?)
  }
}