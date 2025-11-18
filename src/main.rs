use std::{net::SocketAddr, str::FromStr, vec};


use crate::{core::{block::Block, economics::{DEV_WALLET, calculate_dev_fee, get_block_reward}, transaction::{Transaction, TransactionOutput}}, nodev2::{message::Message, node::Node}};


mod core;
mod node;
mod nodev2;
mod version;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
  let args = std::env::args().collect::<Vec<_>>();
  let mut peers: Vec<String> = vec![];
  let mut blockchain_path = "./blockchain1";
  for arg in args.iter().enumerate() {
    if arg.1 == "--peers" && args.get(arg.0 + 1).is_some() {
      peers = args[arg.0 + 1].split(",").map(|s| s.to_string()).collect();
    }
    if arg.1 == "--bc-path" && args.get(arg.0 + 1).is_some() {
      blockchain_path = &args[arg.0 + 1];
    }
  }

  let node = Node::new(blockchain_path);
  let handle = Node::init(node.clone(), peers.iter().map(|seed| SocketAddr::from_str(&seed).unwrap()).collect()).await?;
  
  let block = {
    let blockchain = &node.read().await.blockchain;
    let reward = get_block_reward(blockchain.get_height());
    let mut block = Block::new_block_now(vec![
      Transaction::new_transaction_now(vec![], vec![
        TransactionOutput {
          amount: calculate_dev_fee(reward),
          receiver: DEV_WALLET
        },
        TransactionOutput {
          amount: reward - calculate_dev_fee(reward),
          receiver: DEV_WALLET
        }
      ], &mut vec![])?
    ], &blockchain.get_difficulty_manager().block_difficulty, &blockchain.get_difficulty_manager().tx_difficulty);
    block.transactions[0].compute_pow(&blockchain.get_difficulty_manager().tx_difficulty)?;
    block.compute_pow()?;
    block
  };

  Node::send_to_peers(node, Message::new(nodev2::message::Command::NewBlock { block })).await;

  println!("Node done: {:?}", handle.await);

  Ok(())
}
