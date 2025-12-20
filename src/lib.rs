use bincode::error::EncodeError;
use thiserror::Error;

use core::{
    block::Block,
    blockchain::BlockchainError,
    economics::{DEV_WALLET, calculate_dev_fee, get_block_reward},
    transaction::{Transaction, TransactionInput, TransactionOutput},
};

/// Snap Coin API for accessing blockchain data from different programs
// pub mod api;

/// Provides a standardized way to access blockchain data from many sources
pub mod blockchain_data_provider;

/// Core implementation of Snap Coin, contains all blockchain logic
pub mod core;

/// Contains all crypto primitives
pub mod crypto;

/// Node struct for hosting a P2P Snap Coin node
pub mod nodev2;

/// Tests
mod tests;

/// Current Snap Coin version
pub mod version;

/// Economics package. Mostly utils and CONSTS
pub use core::economics;
pub use core::economics::to_snap;
pub use economics::to_nano;

use crate::{
    blockchain_data_provider::{BlockchainDataProvider, BlockchainDataProviderError},
    core::{block::MAX_TRANSACTIONS, transaction::MAX_TRANSACTION_IO},
    crypto::keys::{Private, Public},
    economics::GENESIS_PREVIOUS_BLOCK_HASH,
};

#[derive(Error, Debug)]
pub enum UtilError {
    #[error("Blockchain error: {0}")]
    BlockchainError(#[from] BlockchainError),

    #[error("Insufficient funds to complete operation")]
    InsufficientFunds,

    #[error("Encode error {0}")]
    EncodeError(#[from] EncodeError),

    #[error("Data provider error {0}")]
    BlockchainDataProviderError(#[from] BlockchainDataProviderError),

    #[error(
        "Too many inputs and outputs for one transaction. Consider splitting transaction in to more than one (smaller SNAP amount) or less receivers."
    )]
    TooMuchIO,

    #[error("Too many transactions for block")]
    TooManyTransactions,
}

/// Build a new transactions, sending from sender to receiver where each receiver has a amount to receive attached. Takes biggest coins first.
/// WARNING: this does not compute transaction pow!
pub async fn build_transaction<B>(
    blockchain_data_provider: &B,
    sender: Private,
    mut receivers: Vec<(Public, u64)>,
    ignore_inputs: Vec<TransactionInput>
    
) -> Result<Transaction, UtilError>
where
    B: BlockchainDataProvider,
{
    let target_balance = receivers
        .iter()
        .fold(0u64, |acc, receiver| acc + receiver.1);

    let mut available_inputs = blockchain_data_provider
        .get_available_transaction_outputs(sender.to_public())
        .await?;

    available_inputs.retain(|(transaction, _, index)| !ignore_inputs.iter().any(|i_input| i_input.output_index == *index && i_input.transaction_id == *transaction));

    let mut used_inputs = vec![];

    let mut current_funds = 0u64;
    for (transaction, input, index) in available_inputs {
        current_funds += input.amount;
        used_inputs.push((transaction, input, index));
        if current_funds >= target_balance {
            break;
        }
    }

    if target_balance > current_funds {
        return Err(UtilError::InsufficientFunds);
    }

    if target_balance < current_funds {
        receivers.push((sender.to_public(), current_funds - target_balance));
    }

    if used_inputs.len() + receivers.len() > MAX_TRANSACTION_IO {
        return Err(UtilError::TooMuchIO);
    }

    used_inputs.sort_by(|a, b| a.1.amount.cmp(&b.1.amount)); // From highest amount to lowest amount (breadcrumbs last)

    let transaction = Transaction::new_transaction_now(
        used_inputs
            .iter()
            .map(|input| TransactionInput {
                transaction_id: input.0,
                output_index: input.2,
                signature: None,
            })
            .collect::<Vec<TransactionInput>>(),
        receivers
            .iter()
            .map(|receiver| TransactionOutput {
                amount: receiver.1,
                receiver: receiver.0,
            })
            .collect(),
        &mut vec![sender; used_inputs.len()],
    )?;

    Ok(transaction)
}

/// Build a new block, given a blockchain data provider reference and a transaction vector
/// WARNING: This does not compute block pow nor hash!
/// WARNING: It is assumed that all input transactions are fully valid (at current blockchain height)
/// WARNING: This function adds reward transactions for you!
pub async fn build_block<B>(
    blockchain_data_provider: &B,
    transactions: &Vec<Transaction>,
    miner: Public,
) -> Result<Block, UtilError>
where
    B: BlockchainDataProvider,
{
    let reward = get_block_reward(blockchain_data_provider.get_height().await?);
    let mut transactions = transactions.clone();
    transactions.push(Transaction::new_transaction_now(
        vec![],
        vec![
            TransactionOutput {
                amount: calculate_dev_fee(reward),
                receiver: DEV_WALLET,
            },
            TransactionOutput {
                amount: reward - calculate_dev_fee(reward),
                receiver: miner,
            },
        ],
        &mut vec![],
    )?);
    if transactions.len() > MAX_TRANSACTIONS {
        return Err(UtilError::TooManyTransactions);
    }
    let reward_tx_i = transactions.len() - 1;
    transactions[reward_tx_i].compute_pow(
        &blockchain_data_provider
            .get_transaction_difficulty()
            .await?,
        None,
    )?;
    let block = Block::new_block_now(
        transactions,
        &blockchain_data_provider.get_block_difficulty().await?,
        &blockchain_data_provider
            .get_transaction_difficulty()
            .await?,
        blockchain_data_provider
            .get_block_hash_by_height(
                blockchain_data_provider
                    .get_height()
                    .await?
                    .saturating_sub(1),
            )
            .await?
            .unwrap_or(GENESIS_PREVIOUS_BLOCK_HASH),
    );

    Ok(block)
}
