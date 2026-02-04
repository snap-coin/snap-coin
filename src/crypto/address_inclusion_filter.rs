use crate::{core::transaction::Transaction, crypto::keys::Public};
use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Error, Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub enum AddressInclusionFilterError {
    #[error("Failed to encode bloom filter")]
    BloomEncoding,

    #[error("Failed to decode bloom filter")]
    BloomDecoding,
}

const FALSE_POSITIVE_RATE: f64 = 0.001;

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode, PartialEq, Hash)]
pub struct AddressInclusionFilter {
    bits: Vec<u8>,
    num_bits: usize,
    num_hashes: u32,
}

impl AddressInclusionFilter {
    pub fn create_filter(
        transactions: &[Transaction],
    ) -> Result<Self, AddressInclusionFilterError> {
        let address_count: usize = transactions.iter().map(|tx| tx.address_count()).sum();

        if address_count == 0 {
            // Handle empty transaction list
            return Ok(Self {
                bits: vec![],
                num_bits: 0,
                num_hashes: 0,
            });
        }

        let num_bits = (-(address_count as f64 * FALSE_POSITIVE_RATE.ln()) / (2f64.ln().powi(2)))
            .ceil() as usize;
        let num_hashes = ((num_bits as f64 / address_count as f64) * 2f64.ln()).ceil() as u32;

        let mut filter = Self {
            bits: vec![0u8; (num_bits + 7) / 8],
            num_bits,
            num_hashes,
        };

        for tx in transactions {
            Self::insert_transaction(&mut filter, tx);
        }

        Ok(filter)
    }

    pub fn search_filter(&self, address: Public) -> Result<bool, AddressInclusionFilterError> {
        Ok(self.contains(&*address))
    }

    fn insert(&mut self, address: &[u8; 32]) {
        if self.num_bits == 0 {
            return;
        } // no-op for empty filter
        for i in 0..self.num_hashes {
            let bit = self.hash(address, i) % self.num_bits as u64;
            self.bits[bit as usize / 8] |= 1 << (bit % 8);
        }
    }

    fn contains(&self, address: &[u8; 32]) -> bool {
        if self.num_bits == 0 {
            return false;
        } // always false for empty filter
        for i in 0..self.num_hashes {
            let bit = self.hash(address, i) % self.num_bits as u64;
            if self.bits[bit as usize / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }

    fn insert_transaction(bloom: &mut Self, tx: &Transaction) {
        for input in &tx.inputs {
            bloom.insert(&*input.output_owner);
        }
        for output in &tx.outputs {
            bloom.insert(&*output.receiver);
        }
    }

    fn hash(&self, address: &[u8; 32], i: u32) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(address);
        hasher.update(&i.to_le_bytes());
        let result = hasher.finalize();
        u64::from_le_bytes(result[0..8].try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::transaction::{Transaction, TransactionInput, TransactionOutput};
    use crate::crypto::keys::Public;

    fn random_public_key() -> Public {
        let mut key = [0u8; 32];
        for byte in key.iter_mut() {
            *byte = rand::random();
        }
        Public::new_from_buf(&key)
    }

    fn create_dummy_tx(addresses: Vec<Public>) -> Transaction {
        let inputs: Vec<TransactionInput> = addresses
            .iter()
            .map(|addr| TransactionInput {
                transaction_id: crate::crypto::Hash([0u8; 32]),
                output_index: 0,
                signature: None,
                output_owner: *addr,
            })
            .collect();

        let outputs: Vec<TransactionOutput> = addresses
            .iter()
            .map(|addr| TransactionOutput {
                amount: 1,
                receiver: *addr,
            })
            .collect();

        Transaction {
            inputs,
            outputs,
            transaction_id: None,
            nonce: 0,
            timestamp: 0,
        }
    }

    #[test]
    fn test_filter_creation_and_contains() {
        let addr1 = random_public_key();
        let addr2 = random_public_key();
        let tx1 = create_dummy_tx(vec![addr1]);
        let tx2 = create_dummy_tx(vec![addr2]);

        let filter = AddressInclusionFilter::create_filter(&[tx1.clone(), tx2.clone()]).unwrap();

        assert!(filter.contains(&*addr1));
        assert!(filter.contains(&*addr2));

        let unknown_addr = random_public_key();
        // Bloom filter may produce false positives, but very unlikely with our parameters
        assert!(!filter.contains(&*unknown_addr));
    }

    #[test]
    fn test_search_filter() {
        let addr = random_public_key();
        let tx = create_dummy_tx(vec![addr]);
        let filter = AddressInclusionFilter::create_filter(&[tx]).unwrap();

        assert_eq!(filter.search_filter(addr).unwrap(), true);
        let unknown_addr = random_public_key();
        assert_eq!(filter.search_filter(unknown_addr).unwrap(), false);
    }

    #[test]
    fn test_multiple_addresses_in_one_tx() {
        let addr1 = random_public_key();
        let addr2 = random_public_key();
        let tx = create_dummy_tx(vec![addr1, addr2]);

        let filter = AddressInclusionFilter::create_filter(&[tx]).unwrap();
        assert!(filter.contains(&*addr1));
        assert!(filter.contains(&*addr2));
    }

    #[test]
    fn test_empty_transactions() {
        let filter = AddressInclusionFilter::create_filter(&[]).unwrap();
        // No addresses, so filter should not contain any random address
        let addr = random_public_key();
        assert!(!filter.contains(&*addr));
    }
}
