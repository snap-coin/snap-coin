use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use log::info;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use crate::full_node::node_state::SharedNodeState;
use crate::{
    core::block::Block,
    full_node::SharedBlockchain,
    node::{
        message::{Command, Message},
        peer::PeerHandle,
    },
};

const IBD_SAFE_SKIP_TX_HASHING: usize = 500;
const BLOCKS_PER_REQUEST: usize = 16; // Like Bitcoin's inv messages
const MAX_BLOCKS_IN_FLIGHT: usize = 1024; // Maximum blocks downloading at once
const STATUS_EVERY: usize = 500;

pub async fn ibd_blockchain(
    node_state: SharedNodeState,
    blockchain: SharedBlockchain,
    full_ibd: bool,
) -> Result<(), anyhow::Error> {
    info!("Starting initial block download");

    let local_height = blockchain.block_store().get_height();

    // Get initial peer for metadata
    let initial_peer = {
        let peers = node_state.connected_peers.read().await;
        if peers.is_empty() {
            return Err(anyhow!("No connected peers available for IBD"));
        }
        peers.values().next().unwrap().clone()
    };

    // Fetch remote height
    let remote_height = match initial_peer
        .request(Message::new(Command::Ping {
            height: local_height,
        }))
        .await?
        .command
    {
        Command::Pong { height } => height,
        _ => return Err(anyhow!("Could not fetch peer height to sync blockchain")),
    };

    if remote_height <= local_height {
        info!("[SYNC] Already synced");
        return Ok(());
    }

    // Fetch block hashes
    let hashes = match initial_peer
        .request(Message::new(Command::GetBlockHashes {
            start: local_height,
            end: remote_height,
        }))
        .await?
        .command
    {
        Command::GetBlockHashesResponse { block_hashes } => block_hashes,
        _ => {
            return Err(anyhow!(
                "Could not fetch peer block hashes to sync blockchain"
            ));
        }
    };

    info!("[SYNC] Fetched {} block hashes", hashes.len());

    let total = hashes.len();
    let next_request_index = Arc::new(AtomicUsize::new(0));
    let blocks_in_flight = Arc::new(AtomicUsize::new(0));
    let blocks_applied = Arc::new(AtomicUsize::new(0));
    let start_time = Instant::now();

    // Channel for downloaded blocks (index, block)
    let (block_tx, mut block_rx) = mpsc::channel::<(usize, Block)>(MAX_BLOCKS_IN_FLIGHT);

    // Download loop - continuously feed blocks to peers
    let downloader = {
        let node_state = node_state.clone();
        let hashes = hashes.clone();
        let next_request_index = next_request_index.clone();
        let blocks_in_flight = blocks_in_flight.clone();

        tokio::spawn(async move {
            loop {
                // Check if we need more blocks in flight
                let in_flight = blocks_in_flight.load(Ordering::SeqCst);
                if in_flight >= MAX_BLOCKS_IN_FLIGHT {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }

                // Get next block index to request
                let start_index = next_request_index.load(Ordering::SeqCst);
                if start_index >= total {
                    break; // All blocks requested
                }

                // Calculate how many blocks to request
                let available_slots = MAX_BLOCKS_IN_FLIGHT - in_flight;
                let blocks_to_request = available_slots
                    .min(BLOCKS_PER_REQUEST)
                    .min(total - start_index);

                // Reserve these blocks
                let end_index = start_index + blocks_to_request;
                next_request_index.store(end_index, Ordering::SeqCst);
                blocks_in_flight.fetch_add(blocks_to_request, Ordering::SeqCst);

                // Get available peers
                let peers: Vec<PeerHandle> = {
                    let peers = node_state.connected_peers.read().await;
                    if peers.is_empty() {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    peers.values().cloned().collect()
                };

                // Distribute blocks across peers round-robin
                for (i, index) in (start_index..end_index).enumerate() {
                    let peer = peers[i % peers.len()].clone();
                    let hash = hashes[index];
                    let block_tx = block_tx.clone();
                    let blocks_in_flight = blocks_in_flight.clone();

                    tokio::spawn(async move {
                        let result = async {
                            let resp = peer
                                .request(Message::new(Command::GetBlock { block_hash: hash }))
                                .await?;

                            match resp.command {
                                Command::GetBlockResponse { block } => block.ok_or_else(|| {
                                    anyhow!("Peer returned empty block {}", hash.dump_base36())
                                }),
                                _ => Err(anyhow!(
                                    "Unexpected response for block {}",
                                    hash.dump_base36()
                                )),
                            }
                        }
                        .await;

                        match result {
                            Ok(block) => {
                                let _ = block_tx.send((index, block)).await;
                            }
                            Err(e) => {
                                info!("[IBD] Failed to download block {}: {}", index, e);
                                // Block will be re-requested when we detect it's missing
                            }
                        }

                        blocks_in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
                }

                // Small delay to prevent tight loop
                tokio::time::sleep(Duration::from_millis(1)).await;
            }

            drop(block_tx);
        })
    };

    // Apply loop - apply blocks in order
    let applier = {
        let blockchain = blockchain.clone();
        let blocks_applied = blocks_applied.clone();
        let blocks_in_flight = blocks_in_flight.clone();

        tokio::spawn(async move {
            let mut next_to_apply = 0;
            let mut buffer: BTreeMap<usize, Block> = BTreeMap::new();
            let mut apply_times = Vec::new();
            let mut last_status_time = Instant::now();

            while next_to_apply < total {
                // Receive blocks
                if let Some((index, block)) = block_rx.recv().await {
                    buffer.insert(index, block);

                    // Apply all consecutive blocks
                    while let Some(block) = buffer.remove(&next_to_apply) {
                        let apply_start = Instant::now();

                        let blockchain_clone = blockchain.clone();
                        let blocks_remaining = total - next_to_apply;
                        spawn_blocking(move || {
                            blockchain_clone.add_block(
                                block,
                                blocks_remaining > IBD_SAFE_SKIP_TX_HASHING && !full_ibd,
                            )
                        })
                        .await
                        .map_err(|e| anyhow!("Blocking task error: {}", e))??;

                        let apply_time = apply_start.elapsed();
                        apply_times.push(apply_time.as_millis() as f64);
                        if apply_times.len() > 100 {
                            apply_times.remove(0);
                        }

                        let finished = blocks_applied.fetch_add(1, Ordering::SeqCst) + 1;
                        next_to_apply += 1;

                        // Status every 500 blocks or every 5 seconds
                        let should_print = finished % STATUS_EVERY == 0
                            || finished == total
                            || last_status_time.elapsed() > Duration::from_secs(5);

                        if should_print {
                            last_status_time = Instant::now();

                            let elapsed = start_time.elapsed().as_secs_f64().max(0.001);
                            let speed = finished as f64 / elapsed;
                            let remaining = total - finished;
                            let eta_secs = (remaining as f64 / speed).max(0.0);
                            let pct = (finished as f64 / total as f64) * 100.0;

                            let avg_apply_ms = if !apply_times.is_empty() {
                                apply_times.iter().sum::<f64>() / apply_times.len() as f64
                            } else {
                                0.0
                            };

                            let in_flight = blocks_in_flight.load(Ordering::SeqCst);

                            info!(
                                "[IBD] {:.1}% ({}/{}) | {:.1} blk/s | ETA: {}s | in-flight: {} | buffered: {} | avg-apply: {:.0}ms",
                                pct,
                                finished,
                                total,
                                speed,
                                eta_secs as u64,
                                in_flight,
                                buffer.len(),
                                avg_apply_ms
                            );
                        }
                    }
                } else {
                    break;
                }
            }

            if next_to_apply < total {
                return Err(anyhow!(
                    "IBD incomplete: applied {}/{} blocks",
                    next_to_apply,
                    total
                ));
            }

            Ok::<(), anyhow::Error>(())
        })
    };

    // Wait for both loops
    let (download_result, apply_result) = tokio::join!(downloader, applier);

    download_result.map_err(|e| anyhow!("Downloader task panicked: {}", e))?;
    apply_result.map_err(|e| anyhow!("Applier task panicked: {}", e))??;

    info!("[SYNC] Blockchain synced successfully");

    Ok(())
}
