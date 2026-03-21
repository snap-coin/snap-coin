use serde::{Deserialize, Serialize};

use crate::core::{block::Block, transaction::{Transaction, TransactionId}};

#[derive(Serialize, Deserialize, Clone)]
pub enum ChainEvent {
    Block { block: Block },
    Transaction { transaction: Transaction },
    TransactionExpiration { transaction: TransactionId },
}
