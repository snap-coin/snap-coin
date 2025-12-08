use bincode::{Decode, Encode, error::EncodeError};
use num_bigint::BigUint;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::crypto::{
    Hash, Signature,
    keys::{Private, Public},
};

/// A way of finding this transaction. Alias for Hash
pub type TransactionId = Hash;

pub const MAX_TRANSACTION_IO: usize = 500;

/// A transaction input, that are funding a set transaction output, that must exist in the current utxo set
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransactionInput {
    pub transaction_id: TransactionId,
    pub output_index: usize,
    pub signature: Option<Signature>,
}

/// A transaction output, specifying the transactions set receiver
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransactionOutput {
    pub amount: u64,
    pub receiver: Public,
}

/// A transaction containing transaction inputs (funding this transaction) and outputs (spending this transactions funds)
/// The transaction id is a way of finding this transaction once it becomes part of this blockchain. It is also the transaction id that is the actual Hash of the transaction buffer obtained by get_tx_hashing_buf
/// The timestamp is set by the sender, and only loosely validated
/// The nonce is also set by the sender to allow the transaction to be mined (POW)
#[derive(Encode, Decode, Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
    pub transaction_id: Option<TransactionId>,
    pub nonce: u64,
    pub timestamp: u64,
}

impl Transaction {
    /// Create a new transaction timestamped now with a set of inputs and outputs. Signed automatically with the signing_keys (in same order as transaction inputs)
    /// WARNING: The transaction still needs to be mined (POW)! compute_pow() must be called after
    pub fn new_transaction_now(
        inputs: Vec<TransactionInput>,
        outputs: Vec<TransactionOutput>,
        signing_keys: &mut Vec<Private>,
    ) -> Result<Self, EncodeError> {
        let mut transaction = Self {
            inputs,
            outputs,
            transaction_id: None,
            nonce: 0,
            timestamp: chrono::Utc::now().timestamp() as u64,
        };
        let signing_buf = transaction.get_input_signing_buf()?;

        for (input, key) in transaction.inputs.iter_mut().zip(signing_keys.iter_mut()) {
            input.signature = Some(Signature::new_signature(key, &signing_buf));
        }

        Ok(transaction)
    }

    /// Mine this transaction (aka. compute POW to allow it to be placed in the network)
    /// WARNING: This is single threaded and needs to complete before a new block is mined on the network as otherwise tx_difficulty becomes invalid, and so the transaction too
    /// Difficulty margin can be used to prevent the behavior mentioned above, however it will lead to a longer compute time. The bugger difficulty margin is, the more likely is the transaction to be valid next block.
    /// Difficulty margin is a multiplier by how much harder does the actual difficulty need to be
    pub fn compute_pow(
        &mut self,
        tx_difficulty: &[u8; 32],
        difficulty_margin: Option<f64>,
    ) -> Result<(), EncodeError> {
        let mut target = BigUint::from_bytes_be(tx_difficulty);

        if let Some(margin) = difficulty_margin {
            if margin > 0.0 {
                // Larger margin -> target becomes smaller → more difficult
                target /= BigUint::from((margin * 1000.0) as u64);
            }
        }
        let mut rng = rand::rng();
        loop {
            self.nonce = rng.random();
            let hashing_buf = self.get_tx_hashing_buf()?;
            if BigUint::from_bytes_be(&*Hash::new(&hashing_buf)) <= target {
                self.transaction_id = Some(Hash::new(&hashing_buf));
                return Ok(());
            }
        }
    }

    /// Get the buffer that needs to be signed by each inputs private key
    pub fn get_input_signing_buf(&self) -> Result<Vec<u8>, EncodeError> {
        let mut signature_less_transaction: Transaction = self.clone();

        signature_less_transaction.transaction_id = None;
        signature_less_transaction.nonce = 0;

        for input in &mut signature_less_transaction.inputs {
            input.signature = None; // remove all signatures for signing
        }

        Ok(bincode::encode_to_vec(
            signature_less_transaction,
            bincode::config::standard(),
        )?)
    }

    /// Get the buffer that needs to be hashed to compute the pow puzzle
    pub fn get_tx_hashing_buf(&self) -> Result<Vec<u8>, EncodeError> {
        let mut signature_less_transaction = self.clone();

        signature_less_transaction.transaction_id = None;

        Ok(bincode::encode_to_vec(
            signature_less_transaction,
            bincode::config::standard(),
        )?)
    }
}
