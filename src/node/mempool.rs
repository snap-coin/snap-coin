use std::{collections::BTreeMap, sync::Arc, time::Duration};

use tokio::{sync::RwLock, time::sleep};

use crate::{
    core::transaction::{Transaction, TransactionId},
    economics::EXPIRATION_TIME,
};

pub struct MemPool {
    /// BTreeMap of expiry timestamp -> transactions
    pending: Arc<RwLock<BTreeMap<u64, Vec<Transaction>>>>,
}

impl MemPool {
    pub fn new() -> Self {
        MemPool {
            pending: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Starts a background task that removes expired transactions
    pub fn start_expiry_watchdog(
        &self,
        mut on_expiry: impl FnMut(TransactionId) + Send + Sync + 'static,
    ) {
        let pending = self.pending.clone();
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_millis(500)).await;
                let now = chrono::Utc::now().timestamp() as u64;

                let mut write_guard = pending.write().await;

                // Remove all expired transactions efficiently
                let expired_keys: Vec<u64> = write_guard.range(..=now).map(|(&k, _)| k).collect();

                for key in expired_keys {
                    if let Some(txs) = write_guard.remove(&key) {
                        for tx in txs {
                            if let Some(tx_id) = tx.transaction_id {
                                on_expiry(tx_id);
                            }
                        }
                    }
                }
            }
        });
    }

    /// Get a vector of all transactions in this mempool
    pub async fn get_mempool(&self) -> Vec<Transaction> {
        self.pending
            .read()
            .await
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect()
    }

    /// Add a transaction to the mempool
    /// WARNING: Make sure this transaction is valid before
    pub async fn add_transaction(&self, transaction: Transaction) {
        let expiry = chrono::Utc::now().timestamp() as u64 + EXPIRATION_TIME;

        let mut write_guard = self.pending.write().await;
        write_guard.entry(expiry).or_default().push(transaction);
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

    /// Remove transactions that have been spent
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

        // Clean up empty expiry buckets
        pending.retain(|_, txs| !txs.is_empty());
    }

    pub async fn mempool_size(&self) -> usize {
        self.pending
            .read()
            .await
            .values()
            .fold(0, |acc, txs| acc + txs.len())
    }

    pub async fn clear(&self) {
        self.pending.write().await.clear();
    }
}
