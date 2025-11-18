# Genral Structure
`main.rs` most imports, entry point
`transaction.rs` contains transaction structure
`utxo.rs` contains utxo's and transaction validation

# Transaction Logic
Transactions are fee less, but to prevent miss use, every sender must complete a PoW puzzle. This includes hasing the whole transaction repeatedly and changing the nonce to a random u64 until it matches a difficulty set by a target amount of tx per block

pub struct TransactionInput {
  pub transaction_id: TransactionId,
  pub output_index: usize,
  pub signature: Option<Signature>
}

pub struct TransactionOutput {
  pub amount: u64,
  pub receiver: Public
}

pub struct Transaction {
  pub inputs: Vec<TransactionInput>,
  pub outputs: Vec<TransactionOutput>,
  pub transaction_id: Option<TransactionId>,
  pub nonce: u64,
  pub timestamp: u64
}

Hashing transaction: hash full transaction 