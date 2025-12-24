use rand::random;

use crate::{
    api::{api_server::Server, client::Client},
    blockchain_data_provider::BlockchainDataProvider,
    build_block, build_transaction,
    crypto::keys::Private,
    full_node::{SharedBlockchain, accept_block, accept_transaction, create_full_node, node_state::SharedNodeState},
    to_nano,
};

async fn reset_bc(blockchain: &SharedBlockchain) {
    let height = blockchain.block_store().get_height();
    for _ in 0..height {
        match blockchain.pop_block() {
            Ok(()) => {}
            Err(e) => panic!("{}", e),
        }
    }
}

async fn test_mempool(
    blockchain: &SharedBlockchain,
    node_state: &SharedNodeState,
) -> Result<(), anyhow::Error> {
    let private1 = Private::new_random();
    let public1 = private1.to_public();

    let private2 = Private::new_random();
    let public2 = private2.to_public();

    let mut genesis_block = build_block(&**blockchain, &vec![], public1).await?;
    #[allow(deprecated)]
    genesis_block.compute_pow()?;
    accept_block(blockchain, node_state, genesis_block).await?;

    let mut some_tx = build_transaction(
        &**blockchain,
        private1,
        vec![(public2, to_nano(10.0))],
        vec![],
    )
    .await?;

    some_tx.compute_pow(&blockchain.get_transaction_difficulty(), None)?;

    assert!(
        node_state.mempool.get_mempool().await.is_empty(),
        "Mempool should be empty"
    );
    accept_transaction(blockchain, node_state, some_tx).await?;
    assert_eq!(
        node_state.mempool.get_mempool().await.len(),
        1,
        "Transaction did not add to mempool"
    );

    let mut some_block = build_block(
        &**blockchain,
        &node_state.mempool.get_mempool().await.clone(),
        public1,
    )
    .await?;
    {
        #[allow(deprecated)]
        some_block.compute_pow()?;
    }

    accept_block(blockchain, node_state, some_block).await?;
    assert!(
        node_state.mempool.get_mempool().await.is_empty(),
        "Mempool should be empty, is: {:#?}",
        node_state.mempool.get_mempool().await
    );

    Ok(())
}

async fn test_api(
    blockchain: &SharedBlockchain,
    node_state: &SharedNodeState,
) -> Result<(), anyhow::Error> {
    let private1 = Private::new_random();
    let public1 = private1.to_public();

    // Create and add a genesis block
    let txs = vec![];
    let mut genesis_block = build_block(&**blockchain, &txs, public1).await?;
    #[allow(deprecated)]
    genesis_block.compute_pow()?;
    accept_block(blockchain, node_state, genesis_block).await?;

    // Create api server & client
    let api_port = 8571u32;
    let api = Server::new(api_port, blockchain.clone(), node_state.clone());
    api.listen().await?;

    let client = Client::connect(format!("127.0.0.1:{}", api_port).parse().unwrap()).await?;

    // Create some transaction
    let mut some_tx = build_transaction(&client, private1, vec![(public1, 100)], vec![]).await?;
    some_tx.compute_pow(&client.get_transaction_difficulty().await?, None)?;

    // Submit this tx
    client.submit_transaction(some_tx).await??;
    assert_eq!(
        node_state.mempool.get_mempool().await.len(),
        1,
        "Transaction not properly processed by node"
    ); // Make sure transaction is added

    // Create some block (fetch mempool back from client)
    let txs = client.get_mempool().await?;
    assert_eq!(txs.len(), 1, "Mempool did not properly fetch"); // Make sure transaction is in mempool
    let mut some_block = build_block(&client, &txs, public1).await?;

    #[allow(deprecated)]
    some_block.compute_pow()?;

    client.submit_block(some_block).await??;
    assert_eq!(
        blockchain.block_store().get_height(),
        2,
        "Client did not properly submit block"
    ); // Make sure block is submitted

    Ok(())
}

#[tokio::test]
async fn test_node() -> Result<(), anyhow::Error> {
    let node_path = "/tmp/node-".to_string() + &(random::<u64>()).to_string();
    let (blockchain, node_state) = create_full_node(&node_path, true);

    test_mempool(&blockchain, &node_state).await?;
    reset_bc(&blockchain).await;
    test_api(&blockchain, &node_state).await?;
    reset_bc(&blockchain).await;

    Ok(())
}
