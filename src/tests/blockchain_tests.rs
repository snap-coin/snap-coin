use std::{ collections::HashMap, fs};

use bincode::config;
use num_bigint::BigUint;
use rand::random;

use crate::{
    build_block, build_transaction, core::{
        blockchain::Blockchain,
        difficulty::{STARTING_BLOCK_DIFFICULTY, STARTING_TX_DIFFICULTY},
        transaction::TransactionOutput,
    }, crypto::keys::Private, node::mempool::MemPool
};

fn new_tmp_blockchain() -> Blockchain {
    let bc_path = "/tmp/bc-".to_string() + &(random::<u64>()).to_string();
    if fs::exists(&bc_path).unwrap() {
        fs::remove_dir_all(&bc_path).unwrap();
    }
    Blockchain::new(&bc_path)
}

#[tokio::test]
async fn test_blockchain_rollback() -> Result<(), anyhow::Error> {
    const TEST_HEIGHT: usize = 5;

    let miner = Private::new_random();
    let mut bc = new_tmp_blockchain();

    let mut diff_log = HashMap::new();
    let mut utxos_log = HashMap::new();
    diff_log.insert(
        bc.get_height(),
        (
            bc.get_block_difficulty(),
            bc.get_transaction_difficulty(),
        ),
    );
    utxos_log.insert(bc.get_height(), bc.get_utxos().utxos.clone());
    for _ in 0..TEST_HEIGHT {
        let mut new_block = crate::build_block(&bc, &vec![], miner.to_public()).await?;
        #[allow(deprecated)]
        new_block.compute_pow()?;

        bc.add_block(new_block.clone())?;
        diff_log.insert(
            bc.get_height(),
            (
                bc.get_block_difficulty(),
                bc.get_transaction_difficulty(),
            ),
        );
        let current_utxos = bc.get_utxos().utxos.clone();
        utxos_log.insert(bc.get_height(), current_utxos.clone());
    }

    for _ in 0..TEST_HEIGHT {
        bc.pop_block()?;
        assert_eq!(
            bc.get_block_difficulty(),
            diff_log.get(&bc.get_height()).unwrap().0,
            "Block difficulty did not roll back correctly"
        );

        assert_eq!(
            bc.get_transaction_difficulty(),
            diff_log.get(&bc.get_height()).unwrap().1,
            "TX difficulty did not roll back correctly"
        );
        assert_eq!(
            bc.get_utxos().utxos,
            utxos_log.get(&bc.get_height()).unwrap().clone(),
            "UTXOS did not roll back correctly (height {})", bc.get_height()
        );
    }

    assert_eq!(
        bc.get_utxos().utxos.len(),
        0,
        "UTXOS did not roll back correctly\n{:#?}",
        bc.get_utxos().utxos
    );
    assert_eq!(bc.get_height(), 0, "Height did not roll back correctly");
    assert_eq!(
        bc.get_block_difficulty(),
        STARTING_BLOCK_DIFFICULTY,
        "Block difficulty did not roll back correctly"
    );
    assert_eq!(
        bc.get_transaction_difficulty(),
        STARTING_TX_DIFFICULTY,
        "TX difficulty did not roll back correctly"
    );

    Ok(())
}

/// Check if bincode encodes the same struct into the same binary data each time
#[test]
fn test_bincode() {
    // We will be testing on a TransactionInput for simplicity
    let input = TransactionOutput {
        amount: 10000,
        receiver: Private::new_random().to_public(),
    };

    let test1 = bincode::encode_to_vec(input, config::standard()).unwrap();
    let test2 = bincode::encode_to_vec(input, config::standard()).unwrap();
    assert_eq!(
        test1, test2,
        "Bincode does not create the same binary for two separate encodings of same object"
    );
}


#[tokio::test]
async fn test_transaction_validation() -> Result<(), anyhow::Error> {
    let private = Private::new_random();
    let public = private.to_public();

    let private2 = Private::new_random();
    let public2 = private2.to_public();

    let mut bc = new_tmp_blockchain();

    // Create some genesis block and add it
    let mut block = build_block(&bc, &vec![], public).await?;
    #[allow(deprecated)]
    block.compute_pow()?;
    bc.add_block(block)?;

    let mut valid_tx = build_transaction(&bc, private, vec![(public2, 10)]).await?;
    valid_tx.compute_pow(&bc.get_transaction_difficulty(), None)?;
    bc.get_utxos().validate_transaction(&valid_tx, &BigUint::from_bytes_be(&bc.get_transaction_difficulty()))?;

    // Test tampering
    valid_tx.timestamp = 0; 
    
    assert!(bc.get_utxos().validate_transaction(&valid_tx, &BigUint::from_bytes_be(&bc.get_transaction_difficulty())).is_err(), "Tampered transaction passed (pre re-hashing)");
    
    valid_tx.compute_pow(&bc.get_transaction_difficulty(), None)?; // Recompute POW

    let res = bc.get_utxos().validate_transaction(&valid_tx, &BigUint::from_bytes_be(&bc.get_transaction_difficulty()));
    assert!(res.is_err(), "Tampered transaction passed (post re-hashing)");

    // Create a new valid tx
    // let mut valid_tx = build_transaction(&bc, private, vec![(public2, 10)])?;



    Ok(())
}

#[tokio::test]
async fn test_mempool() -> Result<(), anyhow::Error> {
    let private = Private::new_random();
    let public = private.to_public();

    let mut bc = new_tmp_blockchain();
    let mut mempool = MemPool::new();

    // Create some genesis block and add it
    let mut block = build_block(&bc, &vec![], public).await?;
    #[allow(deprecated)]
    block.compute_pow()?;
    bc.add_block(block)?;

    let mut new_tx = build_transaction(&bc, private, vec![(public, 100)]).await?;
    new_tx.compute_pow(&bc.get_transaction_difficulty(), None)?;

    assert!(mempool.validate_transaction(&new_tx).await, "Transaction invalidly flagged for double spending");
    mempool.add_transaction(new_tx).await;
    let mut new_tx = build_transaction(&bc, private, vec![(public, 100)]).await?;
    new_tx.compute_pow(&bc.get_transaction_difficulty(), None)?;

    assert!(!mempool.validate_transaction(&new_tx).await, "Transaction not flagged for double spending");
    
    // Create an new block and add it (WITHOUT THE MEMPOOL TX!)
    let mut block = build_block(&bc, &mempool.get_mempool().await, public).await?;
    #[allow(deprecated)]
    block.compute_pow()?;
    bc.add_block(block)?;

    assert!(!mempool.validate_transaction(&new_tx).await, "Transaction not flagged for double spending");

    Ok(())
}