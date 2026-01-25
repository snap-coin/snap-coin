use snap_coin::{api::client::Client, blockchain_data_provider::BlockchainDataProvider, build_block, crypto::keys::Private};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let client = Client::connect("127.0.0.1:3003".parse().unwrap()).await?; // Connect to a local node API

    let mut some_block = build_block(&client, &vec![], Private::new_random().to_public()).await?; // Path where the node will be stored, port to host node
    #[allow(deprecated)] // This is deprecated because it only works on a not congested network, with only 1 miner. Okay for creating genesis blocks
    some_block.compute_pow()?;

    client.submit_block(some_block).await??; // NOTE: This function is not part of the BlockchainDataProvider trait
    let last_block_height = client.get_height().await?.saturating_sub(1); // NOTE: using saturating sub because height is a usize to avoid overflow
    println!("Last block: {}", client.get_block_hash_by_height(last_block_height).await?.unwrap().dump_base36()); // unwrap is okay because we know a block exists since we just added one

    Ok(())
}