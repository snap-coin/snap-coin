use crate::{
    blockchain_data_provider::BlockchainDataProvider,
    core::{
        block::Block,
        blockchain::{Blockchain, BlockchainError},
        transaction::{TransactionId, TransactionOutput},
    },
    crypto::Hash,
    economics::get_block_reward,
};

#[async_trait::async_trait]
impl BlockchainDataProvider for Blockchain {
    async fn get_height(
        &self,
    ) -> Result<usize, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.block_store().get_height())
    }

    async fn get_reward(
        &self,
    ) -> Result<u64, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(get_block_reward(self.block_store().get_height()))
    }

    async fn get_block_by_height(
        &self,
        height: usize,
    ) -> Result<Option<Block>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.block_store().get_block_by_height(height))
    }

    async fn get_block_by_hash(
        &self,
        hash: Hash,
    ) -> Result<Option<Block>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.block_store().get_block_by_hash(hash))
    }

    async fn get_height_by_hash(
        &self,
        hash: Hash,
    ) -> Result<Option<usize>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.block_store().get_block_height_by_hash(hash))
    }

    async fn get_block_hash_by_height(
        &self,
        height: usize,
    ) -> Result<Option<Hash>, crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.block_store().get_block_hash_by_height(height))
    }

    async fn get_transaction_difficulty(
        &self,
    ) -> Result<[u8; 32], crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_transaction_difficulty())
    }

    async fn get_block_difficulty(
        &self,
    ) -> Result<[u8; 32], crate::blockchain_data_provider::BlockchainDataProviderError> {
        Ok(self.get_block_difficulty())
    }

    async fn get_available_transaction_outputs(
        &self,
        address: crate::crypto::keys::Public,
    ) -> Result<
        Vec<(TransactionId, TransactionOutput, usize)>,
        crate::blockchain_data_provider::BlockchainDataProviderError,
    > {
        let mut available_outputs = vec![];

        for item in self.get_utxos().db.iter() {
            let (txid_bytes, value) = item.map_err(|e| BlockchainError::UTXOs(e.to_string()))?;
            let txid_str = String::from_utf8(txid_bytes.to_vec())
                .map_err(|e| BlockchainError::UTXOs(e.to_string()))?;
            let txid = TransactionId::new_from_base36(&txid_str)
                .ok_or(BlockchainError::UTXOs("Invalid TransactionId".to_string()))?;

            // Decode the outputs
            let outputs: Vec<Option<TransactionOutput>> =
                bincode::decode_from_slice::<Vec<Option<TransactionOutput>>, _>(
                    &value,
                    bincode::config::standard(),
                )
                .map_err(|e| BlockchainError::UTXOs(e.to_string()))?
                .0;

            for (index, output_opt) in outputs.into_iter().enumerate() {
                if let Some(output) = output_opt {
                    if output.receiver == address {
                        available_outputs.push((txid, output, index));
                    }
                }
            }
        }

        Ok(available_outputs)
    }
}
