# Snap Coin Core
A rust crate for developing with and for Snap Coin

# Some General Tips
1. Default binary to string serialization is `base36` (letters + numbers only).
2. Most helper functions (like `build_block(...)`, `build_transaction(...)`) take a blockchain data provider (this is a trait!), which is a struct that can access basic blockchain data and perform certain fetches etc. These are most commonly `core::Node.blockchain` and `api::Client`.

# Getting started
## Installation
To utilize this library in your project run
```bash
cargo add snap_coin
```

## Utilization
There are **two** main ways of connecting and interacting with a snap coin network

### As a standalone node (***DIFFICULT***)
Your program becomes the node, and you have direct access over its blockchain, peers, connected nodes, status. This is best for when you need **fast, direct access.** This is not recommended if you are creating a program that a user might want to run along side a existing node or if the user would like to create more than one instance of this program.
This is **the fastest approach** as you directly interface with the node, without any translation layer in between.

1.
    Create a new node instance (**WARNING:** Only one instance can exist in a  program at once otherwise a panic will happen!)
    ```rust
    let node: Arc<RwLock<Node>> = Node::new("./node-path", 8998); // Path where the node will be stored, port to host node
    ```
    Notice how `new()` returns a `Arc<RwLock<Node>>` instead of just a node. This is due to the fact that the node is passed around the internal code a lot and this abstractions makes it async safe.
2.
    Call and access functions of the node
    ```rust
    Node::submit_block(node.clone(), some_block).await?;
    println!("Last block: {}", node.read().await.last_seen_block.dump_base_36());
    ```
    Notice how we clone the node, and call a static function on the `Node` struct. **WARNING:** When accessing direct values from the `Node` struct, make sure to call `node.read().await` or `node.write().await` if the internal state of the node will be modified by this operation.
3. Full example:
    ```rust
    use snap_coin::{build_block, crypto::keys::Private, node::node::Node};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::main]
    async fn main() -> Result<(), anyhow::Error> {
        let node: Arc<RwLock<Node>> = Node::new("./node-path", 8998);

        let mut some_block = build_block(&node.read().await.blockchain, &vec![], Private::new_random().to_public()).await?; // Path where the node will be stored, port to host node
        #[allow(deprecated)] // This is deprecated because it only works on a not congested network, with only 1 miner. Okay for creating genesis blocks
        some_block.compute_pow()?;

        Node::submit_block(node.clone(), some_block).await?; // Notice we clone the node, and call a static function on the Node struct
        println!("Last block: {}", node.read().await.last_seen_block.dump_base36());

        Ok(())
    }
    ```

### As a Snap Coin API interface with an existing node (***EASY***)
This approach makes your program a API client to a node that is already hosted (like snap-coin-node) by your user. This approach is slower and does not have full direct access to the node, however, it is a lot more lightweight then the approach mentioned before. This should be used when **there will be more then one instance of this program running**, for example a wallet can just connect to a hosted node (by the user) instead of being its own node. It is important to understand that the node this client will be connecting too must be **100% trusted** as it can modify, spoof, and fake all interactions with this program.

1.
    Connect to a already running (in a separate program) node, that has a Snap Coin API server exposed
    ```rust
    let client = Client::connect("127.0.0.1:3003".parse().unwrap()).await?;
    ```
2.
    Call functions on the client to obtain and change blockchain / node state
    ```rust
    println!("Last block: {}", client.get_block_hash_by_height(last_block_height).await?.unwrap().dump_base36());
    ```
3. Full Example:
    ```rust
    use snap_coin::{api::client::Client, blockchain_data_provider::BlockchainDataProvider, build_block, crypto::keys::Private, node::node::Node};

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
    ```

# Documentation and Development Reference
Available here: [rust.rs](https://docs.rs/snap-coin/latest/)

# Support this Project
You can support this project in many ways, but it is best to just develop and use Snap Coin. I take pride in this project, and by extent in this code. You can help us grow, as Snap Coin is a community effort, by creating apps that utilize Snap Coin, trading, using it as a medium of exchange for things like jobs.

## Help Develop Snap Coin
This repo is always available for pull requests, i highly incentivize reading the code, documentation, and making new features. If you need any help, reach out to me on discord @ice.is.this or via email ice.is.this@gmail.com. Any issues can be reported in the issues tab.

## Join the discord
Join and grow the community: [discord.gg](https://discord.gg/ZK3gMmr3aR)

## Donate
You can buy the developer a coffee and help me keep my motivation:

Snap Coin: 60bamx9k23wcbv6m4xo26h4lsgxwlb83wv2nf1n0hnnatkj9vx

Bitcoin: bc1qacw3l33kzdg96tu268az3chsyrg3pcup0dat9j

Etherum: 0xef98b5ea67248e8a3ee4f4d3674c8ebb2be4e39a

Solana: 8ST3miN2KLdTNXa7pkK1g2qFB3xKzpe1bvCC2QuVBDHG

BNB Smart Chain 0xef98b5ea67248e8a3ee4f4d3674c8ebb2be4e39a