use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{sync::RwLock, time::sleep};

use crate::{
    core::transaction::{Transaction, TransactionId},
    economics::EXPIRATION_TIME,
};

pub struct MemPool {
    /// Hash map of time of expiry and transaction
    pending: Arc<RwLock<HashMap<u64, Vec<Transaction>>>>,
}

impl MemPool {
    pub fn new() -> Self {
        MemPool {
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn start_expiry_watchdog(&mut self) {
        let pending = self.pending.clone();
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs_f64(0.5)).await;
                pending
                    .write()
                    .await
                    .remove(&(chrono::Utc::now().timestamp() as u64));
            }
        });
    }

    /// Get a vector of all transactions in this mempool
    pub async fn get_mempool(&self) -> Vec<Transaction> {
        self.pending
            .read()
            .await
            .values()
            .flat_map(|v| v.iter().map(|tx| tx.clone()))
            .collect()
    }

    /// Add a transaction to the mempool
    /// WARNING: Make sure this transaction is valid before
    pub async fn add_transaction(&mut self, transaction: Transaction) {
        let expiry = chrono::Utc::now().timestamp() as u64 + EXPIRATION_TIME;
        if self.pending.read().await.contains_key(&expiry) {
            self.pending
                .write()
                .await
                .get_mut(&expiry)
                .unwrap()
                .push(transaction);
        } else {
            self.pending.write().await.insert(expiry, vec![transaction]);
        }
    }

    /// Returns true if a transaction is valid (check for double spending)
    pub async fn validate_transaction(&self, transaction: &Transaction) -> bool {
        let mempool = self.get_mempool().await;
        for mempool_transaction in mempool {
            if transaction.inputs.iter().any(|i| {
                mempool_transaction.inputs.iter().any(|mi| {
                    mi.output_index == i.output_index && mi.transaction_id == i.transaction_id
                })
            }) {
                return false;
            }
        }
        true
    }

    pub async fn spend_transactions(&self, transactions: Vec<TransactionId>) {
        let mut pending = self.pending.write().await;

        for txs in pending.values_mut() {
            txs.retain(|mempool_tx| {
                if let Some(id) = mempool_tx.transaction_id {
                    !transactions.contains(&id)
                } else {
                    true
                }
            });
        }

        // Optional: clean up empty expiry buckets
        pending.retain(|_, txs| !txs.is_empty());
    }
}
