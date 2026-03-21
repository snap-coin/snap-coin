use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use futures::future::join_all;
use log::info;
use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;

use crate::core::utils::slice_vec;
use crate::full_node::node_state::SharedNodeState;
use crate::node::message::ConnectionFlags;
use crate::{
    core::block::Block,
    full_node::SharedBlockchain,
    node::message::{Command, Message},
};

pub const IBD_SAFE_SKIP_HASHING: usize = 500;
const DOWNLOAD_BATCH: usize = 2000; // How many blocks to download for all peers
const MAX_PENDING: usize = 1024;

pub async fn ibd_blockchain(
    node_state: SharedNodeState,
    blockchain: SharedBlockchain,
    full_ibd: bool,
    hashing_threads: usize,
) -> Result<(), anyhow::Error> {
    info!("[IBD] Starting initial block download");

    let local_height = blockchain.block_store().get_height();

    // Get initial peer for metadata
    let initial_peer = {
        let peers = node_state.connected_peers.read().await;
        if peers.is_empty() {
            return Err(anyhow!("No connected peers available for IBD"));
        }
        peers
            .values()
            .filter(|p| p.flags.contains(ConnectionFlags::FULL_NODE_CAPABILITY))
            .next()
            .unwrap()
            .clone()
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
        info!("[IBD] Already synced");
        return Ok(());
    }

    // Fetch block hashes
    let hashes = Arc::new(
        match initial_peer
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
        },
    );
    let total = hashes.len();

    info!(
        "[IBD] Fetched {total} block hashes. First hash: {}",
        hashes[0].dump_base36()
    );

    // let total = hashes.len();
    let mut current_processing_height = local_height;
    let (downloaded_block_tx, mut downloaded_block_rx) = mpsc::channel::<Block>(MAX_PENDING);

    let download_stats: Arc<RwLock<(Option<(SocketAddr, f64)>, Option<(SocketAddr, f64)>)>> =
        Arc::new(RwLock::new((None, None)));

    let downloader = {
        let mut downloader_height = 0; // Hash fetch local index
        let node_state = node_state.clone();
        let download_stats = download_stats.clone();
        tokio::spawn(async move {
            let mut peer_download_speeds = HashMap::<SocketAddr, f64>::new();
            loop {
                let mut download_tasks = vec![];
                let average_peer_speed = if peer_download_speeds.is_empty() {
                    1.0
                } else {
                    peer_download_speeds.values().sum::<f64>() / peer_download_speeds.len() as f64
                };

                let peer_batch = DOWNLOAD_BATCH / node_state.connected_peers.read().await.len();
                for (ip, peer) in node_state.connected_peers.read().await.iter() {
                    if !peer.flags.contains(ConnectionFlags::FULL_NODE_CAPABILITY) {
                        continue;
                    }

                    let peer_speed = peer_download_speeds.get(ip).unwrap_or(&average_peer_speed);
                    let peer_contribution = peer_speed / average_peer_speed;
                    let download_end = (downloader_height
                        + (peer_batch as f64 * peer_contribution) as usize)
                        .min(total);
                    let batch = slice_vec(&hashes, downloader_height, download_end).to_vec();

                    let peer = peer.clone();
                    let ip = ip.clone();

                    download_tasks.push(tokio::spawn(async move {
                        let mut speeds = vec![];
                        let mut blocks = vec![];

                        // Prepare a vector of futures
                        let futures: Vec<_> = batch
                            .iter()
                            .map(|hash| {
                                let peer = peer.clone();
                                async move {
                                    let start = Instant::now(); // measure per-block download time
                                    match peer
                                        .request(Message::new(Command::GetBlock {
                                            block_hash: *hash,
                                        }))
                                        .await?
                                        .command
                                    {
                                        Command::GetBlockResponse { block } => {
                                            let elapsed_secs = start.elapsed().as_secs_f64();
                                            let block_size = bincode::encode_to_vec(
                                                &block,
                                                bincode::config::standard(),
                                            )?
                                            .len();
                                            let speed = block_size as f64 / elapsed_secs; // bytes per second
                                            Ok((block, speed))
                                        }
                                        _ => Err(anyhow!("Peer responded with incorrect response")),
                                    }
                                }
                            })
                            .collect();

                        // Run all futures concurrently and wait for them
                        let results = join_all(futures).await;

                        for result in results {
                            if let Ok((Some(block), speed)) = result {
                                blocks.push(block);
                                speeds.push(speed);
                            }
                        }

                        Ok::<(Vec<f64>, SocketAddr, Vec<Block>), anyhow::Error>((
                            speeds, ip, blocks,
                        ))
                    }));

                    downloader_height = download_end;
                }

                let mut fastest: Option<(SocketAddr, f64)> = None;
                let mut slowest: Option<(SocketAddr, f64)> = None;
                for task in download_tasks {
                    let (speeds, ip, blocks) = task.await??; // Check if peer downloaded all blocks correctly
                    for block in blocks {
                        // Send all our blocks
                        downloaded_block_tx.send(block).await?;
                    }
                    if speeds.is_empty() {
                        continue;
                    }
                    let speed = speeds.iter().sum::<f64>() / speeds.len() as f64 * 1000.0;
                    peer_download_speeds.insert(ip, speed);

                    let dummy_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], 0));

                    if fastest.unwrap_or((dummy_addr, 0.0)).1 < speed {
                        fastest = Some((ip, speed));
                    }
                    if slowest.unwrap_or((dummy_addr, f64::INFINITY)).1 > speed {
                        slowest = Some((ip, speed));
                    }
                }
                *download_stats.write().await = (fastest, slowest);
                if downloader_height == total {
                    info!("[IBD] Downloads finished");
                    break;
                }
            }

            Ok::<(), anyhow::Error>(())
        })
    };

    // Validate hashes concurrently
    let (validate_hash_tx, validate_hash_rx) = mpsc::channel::<Block>(hashing_threads);
    let validate_hash_rx = Arc::new(Mutex::new(validate_hash_rx)); // Arc Mutex lock so that when a thread is available to take a new hash it will lock onto it and get the next job

    let hashers = tokio::spawn(async move {
        let mut handles = vec![];

        for _ in 0..hashing_threads {
            let validate_hash_rx = validate_hash_rx.clone();

            handles.push(tokio::task::spawn_blocking(move || {
                loop {
                    let block = {
                        let mut rx = validate_hash_rx.lock().unwrap();
                        rx.blocking_recv()
                    };

                    match block {
                        Some(block) => block.validate_block_hash()?,
                        None => break,
                    }
                }

                Ok::<(), anyhow::Error>(())
            }));
        }

        for handle in handles {
            handle.await??;
        }

        Ok::<(), anyhow::Error>(())
    });

    // Applier loop
    let applier = tokio::spawn(async move {
        let start_time = Instant::now(); // Track total elapsed time
        let total_blocks_to_sync = (remote_height - local_height) as f64; // Only the remaining blocks

        loop {
            if let Some(block) = downloaded_block_rx.recv().await {
                let speed_up = !full_ibd
                    && (remote_height - current_processing_height) > IBD_SAFE_SKIP_HASHING;
                blockchain.add_block(block.clone(), speed_up, speed_up)?;
                if speed_up {
                    // We must validate hash, multi thread
                    validate_hash_tx.send(block).await?
                };

                current_processing_height += 1;

                // Calculate percent completed relative to remaining blocks
                let processed_blocks = (current_processing_height - local_height) as f64;
                let percent = processed_blocks / total_blocks_to_sync * 100.0;

                // Calculate ETA
                let elapsed_secs = start_time.elapsed().as_secs_f64();
                let remaining_blocks = total_blocks_to_sync - processed_blocks;
                let blocks_per_sec = if elapsed_secs > 0.0 {
                    processed_blocks / elapsed_secs
                } else {
                    0.0
                };
                let eta_secs = if blocks_per_sec > 0.0 {
                    remaining_blocks / blocks_per_sec
                } else {
                    f64::INFINITY
                };

                if current_processing_height % 5000 == 0
                    || current_processing_height == remote_height
                {
                    let fastest = if let Some((ip, speed)) = download_stats.read().await.0 {
                        format!("[{ip}, {:.4} kB/s]", speed / 1000.0)
                    } else {
                        "[no info]".to_string()
                    };
                    let slowest = if let Some((ip, speed)) = download_stats.read().await.1 {
                        format!("[{ip}, {:.4} kB/s]", speed / 1000.0)
                    } else {
                        "[no info]".to_string()
                    };

                    info!(
                        "[IBD] Progress: {current_processing_height} / {remote_height} ({percent:.2}%) \
                        Speed: {:.2} blocks/s ETA: {:.2} seconds Fastest peer: {fastest} Slowest peer: {slowest}",
                        blocks_per_sec, eta_secs
                    );
                }

                if current_processing_height == remote_height {
                    break;
                }
            } else {
                break;
            }
        }

        Ok::<(), anyhow::Error>(())
    });

    hashers.await??;
    applier.await??;
    downloader.await??;

    Ok(())
}
