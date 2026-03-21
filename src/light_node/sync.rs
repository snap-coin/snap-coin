use std::{sync::Arc, time::Duration};

use futures::stream::{FuturesUnordered, StreamExt};
use log::info;
use tokio::sync::Semaphore;

use crate::{
    core::{block::BlockMetadata, blockchain::BlockchainError},
    crypto::address_inclusion_filter::AddressInclusionFilterError,
    light_node::{accept_block, light_node_state::SharedLightNodeState, pop_block},
    node::{
        message::{Command, ConnectionFlags, METADATA_FETCH_MAX_COUNT, Message},
        peer::PeerError,
    },
};

const QUORUM_RATIO: f64 = 0.50;
const CHUNK_TIMEOUT_SECS: u64 = 10;
const PEER_PIPELINE_DEPTH: usize = 12;

#[derive(thiserror::Error, Debug)]
pub enum LightNodeSyncError {
    #[error("Blockchain error: {0}")]
    Blockchain(#[from] BlockchainError),
    #[error("No available peers to start syncing")]
    NoPeer,
    #[error("Peer error {0}")]
    PeerError(#[from] PeerError),
    #[error("Address inclusion filter error {0}")]
    AddressInclusionFilter(#[from] AddressInclusionFilterError),
}

async fn quorum_request<F, T>(
    node_state: &SharedLightNodeState,
    message: Message,
    extract: F,
) -> Option<T>
where
    F: Fn(Command) -> Option<T> + Send + Sync + 'static,
    T: Clone + PartialEq + Send + 'static,
{
    let peers: Vec<_> = node_state
        .connected_peers
        .read()
        .await
        .values()
        .filter(|p| p.flags.contains(ConnectionFlags::FULL_NODE_CAPABILITY))
        .cloned()
        .collect();

    if peers.is_empty() {
        return None;
    }

    let quorum = ((peers.len() as f64 * QUORUM_RATIO).ceil() as usize).max(1);
    let extract = Arc::new(extract);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Option<T>>(peers.len());

    for peer in &peers {
        let peer = peer.clone();
        let tx = tx.clone();
        let message = message.clone();
        let extract = extract.clone();
        tokio::spawn(async move {
            let payload = match peer.request(message).await {
                Ok(msg) => extract(msg.command),
                Err(_) => None,
            };
            let _ = tx.send(payload).await;
        });
    }
    drop(tx);

    let mut responses: Vec<T> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(CHUNK_TIMEOUT_SECS);

    while responses.len() < quorum {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(Some(m))) => {
                responses.push(m);
                let best_count = responses
                    .iter()
                    .map(|c| responses.iter().filter(|r| *r == c).count())
                    .max()
                    .unwrap_or(0);
                if best_count >= quorum {
                    break;
                }
            }
            Ok(Some(None)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }

    if responses.is_empty() {
        return None;
    }

    let canonical = responses
        .iter()
        .max_by_key(|c| responses.iter().filter(|r| *r == *c).count())
        .unwrap();

    Some(canonical.clone())
}

async fn fetch_all_metadata_chunks(
    node_state: &SharedLightNodeState,
    chunk_ranges: Vec<(usize, usize)>,
    total_chunks: usize,
) -> Vec<Option<Vec<BlockMetadata>>> {
    let peers: Vec<_> = node_state
        .connected_peers
        .read()
        .await
        .values()
        .filter(|p| p.flags.contains(ConnectionFlags::FULL_NODE_CAPABILITY))
        .cloned()
        .collect();

    if peers.is_empty() {
        return vec![None; chunk_ranges.len()];
    }

    let peer_count = peers.len();
    let quorum = ((peer_count as f64 * QUORUM_RATIO).ceil() as usize).max(1);

    // One semaphore per peer, shared across all chunk tasks.
    let semaphores: Vec<Arc<Semaphore>> = (0..peer_count)
        .map(|_| Arc::new(Semaphore::new(PEER_PIPELINE_DEPTH)))
        .collect();

    let chunk_count = chunk_ranges.len();

    let futs: FuturesUnordered<_> = chunk_ranges
        .into_iter()
        .enumerate()
        .map(|(i, (chunk_start, chunk_end))| {
            let peers = peers.clone();
            let semaphores = semaphores.clone();

            tokio::spawn(async move {
                let (tx, mut rx) =
                    tokio::sync::mpsc::channel::<Option<Vec<BlockMetadata>>>(peers.len());

                for (peer, semaphore) in peers.iter().zip(semaphores.iter()) {
                    let peer = peer.clone();
                    let semaphore = semaphore.clone();
                    let tx = tx.clone();
                    let message = Message::new(Command::GetBlockMetadatas {
                        range_start: chunk_start,
                        range_end: chunk_end,
                    });

                    tokio::spawn(async move {
                        // Acquire the pipeline slot first — deadline only starts
                        // after we have the slot so queued chunks don't time out
                        // while waiting for a free slot.
                        let _permit = semaphore.acquire().await.unwrap();
                        let deadline = tokio::time::Instant::now()
                            + Duration::from_secs(CHUNK_TIMEOUT_SECS);
                        let payload = match tokio::time::timeout_at(
                            deadline,
                            peer.request(message),
                        )
                        .await
                        {
                            Ok(Ok(msg)) => match msg.command {
                                Command::GetBlockMetadatasResponse { block_metadatas } => {
                                    Some(block_metadatas)
                                }
                                _ => None,
                            },
                            _ => None,
                        };
                        let _ = tx.send(payload).await;
                        // _permit drops here, freeing the slot.
                    });
                }
                drop(tx);

                // Outer deadline is generous: permit wait + request time per slot.
                // Each peer can have PEER_PIPELINE_DEPTH chunks ahead of it, so
                // worst case we wait that many timeouts before our slot opens.
                let outer_deadline = tokio::time::Instant::now()
                    + Duration::from_secs(CHUNK_TIMEOUT_SECS * PEER_PIPELINE_DEPTH as u64 + CHUNK_TIMEOUT_SECS);

                let mut responses: Vec<Vec<BlockMetadata>> = Vec::new();

                while responses.len() < quorum {
                    match tokio::time::timeout_at(outer_deadline, rx.recv()).await {
                        Ok(Some(Some(m))) => {
                            responses.push(m);
                            let best_count = responses
                                .iter()
                                .map(|c| responses.iter().filter(|r| *r == c).count())
                                .max()
                                .unwrap_or(0);
                            if best_count >= quorum {
                                break;
                            }
                        }
                        Ok(Some(None)) => {}
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }

                if responses.is_empty() {
                    return (i, chunk_start, chunk_end, 0usize, peer_count, None);
                }

                let canonical = responses
                    .iter()
                    .max_by_key(|c| responses.iter().filter(|r| *r == *c).count())
                    .cloned()
                    .unwrap();

                let agreeing = responses.iter().filter(|r| **r == canonical).count();

                (i, chunk_start, chunk_end, agreeing, peer_count, Some(canonical))
            })
        })
        .collect();

    let mut results = vec![None; chunk_count];

    futures::pin_mut!(futs);
    while let Some(join_result) = futs.next().await {
        if let Ok((i, chunk_start, chunk_end, agreeing, peers_total, result)) = join_result {
            let consensus_pct = (agreeing as f64 / peers_total as f64 * 100.0).round() as usize;
            match &result {
                Some(metadatas) => info!(
                    "[SYNC] Chunk {}/{} synced {}-{} | {} metadatas | consensus {}% ({}/{})",
                    i + 1,
                    total_chunks,
                    chunk_start,
                    chunk_end,
                    metadatas.len(),
                    consensus_pct,
                    agreeing,
                    peers_total,
                ),
                None => info!(
                    "[SYNC] Chunk {}/{} failed quorum {}-{}",
                    i + 1,
                    total_chunks,
                    chunk_start,
                    chunk_end,
                ),
            }
            results[i] = result;
        }
    }

    results
}

pub async fn start_sync(node_state: SharedLightNodeState) -> Result<(), LightNodeSyncError> {
    let first_meta = quorum_request(
        &node_state,
        Message::new(Command::GetBlockMetadatas {
            range_start: node_state.get_height(),
            range_end: node_state.get_height() + 1,
        }),
        |msg| match msg {
            Command::GetBlockMetadatasResponse { block_metadatas } => {
                block_metadatas.into_iter().next().clone()
            }
            _ => None,
        },
    )
    .await;

    if let Some(first_meta) = first_meta
        && node_state.get_height() != 0
    {
        if node_state.last_seen_block_hashes().iter().last() != Some(&first_meta.previous_block) {
            for _ in 0..20 {
                pop_block(&node_state).await?;
            }
        }
    }

    let start_height = node_state.get_height();

    let probe_peer = {
        node_state
            .connected_peers
            .read()
            .await
            .values()
            .next()
            .ok_or(LightNodeSyncError::NoPeer)?
            .clone()
    };

    let remote_height = match probe_peer
        .request(Message::new(Command::Ping {
            height: start_height,
        }))
        .await?
        .command
    {
        Command::Pong { height } => height,
        _ => return Err(LightNodeSyncError::PeerError(PeerError::IncorrectResponse)),
    };

    if start_height >= remote_height {
        info!("[SYNC] Already synced");
        return Ok(());
    }

    info!("[SYNC] Starting sync local height: {start_height} remote height: {remote_height}");

    let mut chunk_ranges = vec![];
    let mut chunk_start = start_height;
    while chunk_start < remote_height {
        let chunk_end = (chunk_start + METADATA_FETCH_MAX_COUNT).min(remote_height);
        chunk_ranges.push((chunk_start, chunk_end));
        chunk_start = chunk_end;
    }

    let total_chunks = chunk_ranges.len();
    let chunk_results = fetch_all_metadata_chunks(&node_state, chunk_ranges, total_chunks).await;

    let mut all_metadatas = vec![];
    for result in chunk_results {
        if let Some(metadatas) = result {
            all_metadatas.extend(metadatas);
        }
    }

    for metadata in all_metadatas {
        let block_hash = metadata
            .hash
            .expect("Quorum returned metadata with no hash");
        let mut interested = false;
        for addr in node_state.utxos.get_watched() {
            if metadata.address_inclusion_filter.search_filter(addr)? {
                interested = true;
                break;
            }
        }

        if interested {
            info!(
                "[SYNC] Interesting block at height {}",
                node_state.get_height()
            );
            if let Some(block) = quorum_request(
                &node_state,
                Message::new(Command::GetBlock { block_hash }),
                |cmd| match cmd {
                    Command::GetBlockResponse { block } => block,
                    _ => None,
                },
            )
            .await
            {
                accept_block(&node_state, block, true).await?;
                node_state.flush().await;
            } else {
                return Err(LightNodeSyncError::PeerError(PeerError::IncorrectResponse));
            }
        } else {
            node_state.increment_height();
        }
    }
    node_state.flush().await;

    Ok(())
}