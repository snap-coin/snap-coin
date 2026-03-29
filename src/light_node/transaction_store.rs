use crate::core::{block_store::TransactionAndInfo, transaction::TransactionId};

pub struct TransactionStore {
    transactions: sled::Tree,
}

impl TransactionStore {
    pub fn new(transaction_tree: sled::Tree) -> Self {
        Self {
            transactions: transaction_tree,
        }
    }

    pub fn get_transaction_and_info(
        &self,
        transaction_id: TransactionId,
    ) -> Option<TransactionAndInfo> {
        Some(
            bincode::decode_from_slice(
                self.transactions
                    .get(&transaction_id.dump_buf())
                    .unwrap()?
                    .as_ref(),
                bincode::config::standard(),
            )
            .unwrap()
            .0,
        )
    }

    pub fn remove_transactions_at_height(&self, height: usize) {
        for item in self.transactions.iter() {
            let (tx_id, tx_buf) = item.unwrap();
            let tx: TransactionAndInfo =
                bincode::decode_from_slice(tx_buf.as_ref(), bincode::config::standard())
                    .unwrap()
                    .0;
            if tx.at_height == height as u64 {
                self.transactions.remove(tx_id).unwrap();
            }
        }
    }

    pub fn remove_transaction(&self, transaction_id: TransactionId) {
        self.transactions
            .remove(&transaction_id.dump_buf())
            .unwrap();
    }

    pub fn put_transaction(&self, tx: TransactionAndInfo) {
        let tx_buf = bincode::encode_to_vec(&tx, bincode::config::standard()).unwrap();
        self.transactions
            .insert(&tx.transaction.transaction_id.unwrap().dump_buf(), tx_buf).unwrap();
    }

    pub fn iter_transactions(&self) -> impl DoubleEndedIterator<Item = TransactionAndInfo> + '_ {
        self.transactions.iter().map(|item| {
            let (_tx_id, tx_buf) = item.unwrap();
            bincode::decode_from_slice(tx_buf.as_ref(), bincode::config::standard())
                .unwrap()
                .0
        })
    }
}
