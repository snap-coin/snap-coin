use bincode::{Decode, Encode, error::EncodeError};
use num_bigint::BigUint;
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto::{
    Hash, Signature,
    keys::{Private, Public},
};

/// A way of finding this transaction. Alias for Hash
pub type TransactionId = Hash;

pub const MAX_TRANSACTION_IO: usize = 150;

#[derive(Error, Debug)]
pub enum TransactionError {
    #[error("{0}")]
    EncodeError(#[from] EncodeError),

    #[error("Transaction missing ID")]
    MissingId,

    #[error("Transaction missing signature/s")]
    MissingSignatures,

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

    #[error("Transaction input output owner is invalid for transaction {0}")]
    IncorrectOutputOwner(String),

    #[error("Double spending detected in the same transaction {0}")]
    DoubleSpend(String),

    #[error("Transaction output amount cannot be zero for transaction {0}")]
    ZeroOutput(String),

    #[error("Transaction inputs and outputs don't sum up to same amount for transaction {0}")]
    SumMismatch(String),

    #[error("Transaction has too many inputs or outputs")]
    TooMuchIO,
    
    #[error("Overflow error")]
    OverflowError,

    #[error("{0}")]
    Other(String),
}

/// A transaction input, that are funding a set transaction output, that must exist in the current utxo set
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Hash)]
pub struct TransactionInput {
    pub transaction_id: TransactionId,
    pub output_index: usize,
    pub signature: Option<Signature>,
    pub output_owner: Public,
}

/// A transaction output, specifying the transactions set receiver
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Hash)]
pub struct TransactionOutput {
    pub amount: u64,
    pub receiver: Public,
}

/// A transaction containing transaction inputs (funding this transaction) and outputs (spending this transactions funds)
/// The transaction id is a way of finding this transaction once it becomes part of this blockchain. It is also the transaction id that is the actual Hash of the transaction buffer obtained by get_tx_hashing_buf
/// The timestamp is set by the sender, and only loosely validated
/// The nonce is also set by the sender to allow the transaction to be mined (POW)
#[derive(Encode, Decode, Debug, Clone, Serialize, Deserialize, PartialEq, Hash)]
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
    /// WARNING: You most likely want to supply this function with the LIVE transaction difficulty, which is adjusted for mempool difficulty pressure
    /// WARNING: This is single threaded and needs to complete before a new block is mined on the network as otherwise tx_difficulty becomes invalid, and so the transaction too. This effect can also be amplified, by mempool pressure, which might tell nodes to reject this transaction if the live transaction difficulty criteria is not met.
    /// Difficulty margin can be used to prevent the behavior mentioned above, however it will lead to a longer PoW compute time.
    /// Difficulty margin is a percentage [0 - 1), where 0 means no change to target difficulty, and 1 means difficulty is 0 (incomputable, ever). It is recommended, that users (if displayed) should be presented with a 0 - 50% logarithmically scaled slider.
    pub fn compute_pow(
        &mut self,
        live_tx_difficulty: &[u8; 32],
        difficulty_margin: Option<f64>,
    ) -> Result<(), EncodeError> {
        let mut target = BigUint::from_bytes_be(live_tx_difficulty);

        if let Some(margin) = difficulty_margin {
            if margin > 0.0 {
                let scale = 1.0 - margin; // smaller target -> harder
                let scale_int = (scale * 1_000_000.0) as u64;
                target *= BigUint::from(scale_int);
                target /= BigUint::from(1_000_000u64);
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

    pub fn check_completeness(&self) -> Result<(), TransactionError> {
        self.transaction_id.ok_or(TransactionError::MissingId)?;
        for input in &self.inputs {
            input.signature.ok_or(TransactionError::MissingSignatures)?;
        }

        Ok(())
    }

    pub fn address_count(&self) -> usize {
        self.inputs.len() + self.outputs.len()
    }

    /// check if this tx contains a address
    pub fn contains_address(&self, address: Public) -> bool {
        for input in &self.inputs {
            if input.output_owner == address {
                return true;
            }
        }
        for output in &self.outputs {
            if output.receiver == address {
                return true;
            }
        }
        false
    }
}
