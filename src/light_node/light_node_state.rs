use sled::Db;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::{
    Mutex, RwLock,
    watch::{self, Ref},
};

use crate::{
    core::{difficulty::{STARTING_BLOCK_DIFFICULTY, calculate_live_transaction_difficulty}, transaction::TransactionId},
    crypto::Hash,
    light_node::{transaction_store::TransactionStore, utxos::LightNodeUTXOs},
    node::{BAN_SCORE_THRESHOLD, ClientHealthScores, PUNISHMENT, mempool::MemPool, peer::PeerHandle},
};

pub type SharedLightNodeState = Arc<LightNodeState>;

pub struct LightNodeState {
    pub utxos: LightNodeUTXOs,
    pub transaction_store: TransactionStore,
    pub connected_peers: RwLock<HashMap<SocketAddr, PeerHandle>>,
    pub mempool: MemPool,
    pub client_health_scores: ClientHealthScores,
    pub is_syncing: AtomicBool,
    pub processing: Mutex<()>,
    pub transaction_difficulty: RwLock<[u8; 32]>,
    last_seen_block_reader: watch::Receiver<Hash>,
    last_seen_block_writer: watch::Sender<Hash>,
    node_db: Db,
    last_seen_transactions_reader: watch::Receiver<VecDeque<TransactionId>>,
    last_seen_transactions_writer: watch::Sender<VecDeque<TransactionId>>,
}

impl LightNodeState {
    pub fn new(node_path: &str) -> Result<SharedLightNodeState, sled::Error> {
        let light_node_db = sled::open(node_path)?;
        
        let (last_seen_block_writer, last_seen_block_reader) =
            watch::channel(Hash::new_from_buf([0u8; 32]));
        let (last_seen_transactions_writer, last_seen_transactions_reader) =
            watch::channel(VecDeque::new());

        Ok(Arc::new(LightNodeState {
            utxos: LightNodeUTXOs::new(&light_node_db)?,
            transaction_store: TransactionStore::new(light_node_db.open_tree("transactions")?),
            connected_peers: RwLock::new(HashMap::new()),
            is_syncing: AtomicBool::new(false),
            processing: Mutex::new(()),
            last_seen_block_writer,
            last_seen_block_reader,
            last_seen_transactions_reader,
            last_seen_transactions_writer,
            node_db: light_node_db,
            mempool: MemPool::new(),
            client_health_scores: RwLock::new(HashMap::new()),
            transaction_difficulty: RwLock::new(STARTING_BLOCK_DIFFICULTY)
        }))
    }

    /// Get current height
    pub fn get_height(&self) -> usize {
        self.node_db
            .get("height")
            .unwrap()
            .map(|v| usize::from_be_bytes(v.as_ref().try_into().unwrap()))
            .unwrap_or(0)
    }

    /// Set current height
    pub fn set_height(&self, height: usize) {
        self.node_db
            .insert("height", height.to_be_bytes().as_ref())
            .unwrap();
    }

    /// Increment height by 1
    pub fn increment_height(&self) {
        let height = self.get_height();
        self.set_height(height + 1);
    }

    /// Decrement height by 1
    pub fn decrement_height(&self) {
        let height = self.get_height();
        self.set_height(height - 1);
    }

    /// Load block hash history from db
    fn load_block_hashes(db: &Db) -> VecDeque<Hash> {
        db.get("block_hashes")
            .unwrap()
            .map(|v| {
                v.chunks(32)
                    .map(|chunk| Hash::new_from_buf(chunk.try_into().unwrap()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Save block hash history to db
    fn save_block_hashes(db: &Db, hashes: &VecDeque<Hash>) {
        let bytes: Vec<u8> = hashes.iter().flat_map(|h| h.dump_buf()).collect();
        db.insert("block_hashes", bytes.as_slice()).unwrap();
    }

    /// Get the recent block hash history (front = newest)
    pub fn last_seen_block_hashes(&self) -> VecDeque<Hash> {
        Self::load_block_hashes(&self.node_db)
    }

    /// Push a new block hash to the front, evicting oldest from back if >1000
    pub fn push_block_hash(&self, hash: Hash) {
        let mut hashes = Self::load_block_hashes(&self.node_db);

        hashes.push_front(hash);

        if hashes.len() > 1000 {
            // We only need to store last 1000 hashes
            hashes.pop_back();
        }

        Self::save_block_hashes(&self.node_db, &hashes);
    }

    /// Get the latest seen block
    /// WARNING: Not persistent
    pub fn last_seen_block(&self) -> Hash {
        self.last_seen_block_reader.borrow().clone()
    }

    /// Set a new last seen block
    pub fn set_last_seen_block(&self, hash: Hash) {
        let _ = self.last_seen_block_writer.send(hash);
    }

    /// Get the latest seen transactions
    pub fn last_seen_transactions(&self) -> Ref<'_, VecDeque<TransactionId>> {
        self.last_seen_transactions_reader.borrow()
    }

    /// Add a new last seen transaction, removing the oldest if >500
    pub fn add_last_seen_transaction(&self, tx_id: TransactionId) {
        let mut transactions: VecDeque<TransactionId> =
            self.last_seen_transactions_reader.borrow().clone();

        // Avoid duplicates
        if !transactions.contains(&tx_id) {
            transactions.push_back(tx_id);

            // Keep only the latest 500 transactions
            if transactions.len() > 500 {
                transactions.pop_front(); // remove oldest
            }

            let _ = self.last_seen_transactions_writer.send(transactions);
        }
    }

    pub async fn get_live_transaction_difficulty(
        &self,
        transaction_difficulty: [u8; 32],
    ) -> [u8; 32] {
        calculate_live_transaction_difficulty(
            &transaction_difficulty,
            self.mempool.mempool_size().await,
        )
    }

    /// Punish a IP address
    pub async fn punish_ip(&self, ip: IpAddr) {
        *self
            .client_health_scores
            .write()
            .await
            .entry(ip)
            .or_insert(0) += PUNISHMENT;
    }

    /// "Forgive" everyone by 1 pt
    pub async fn decrement_punishments(&self) {
        let mut scores = self.client_health_scores.write().await;

        let mut to_remove = Vec::new();

        for (ip, score) in scores.iter_mut() {
            *score = score.saturating_sub(PUNISHMENT);
            if *score == 0 {
                to_remove.push(ip.clone());
            }
        }

        for ip in to_remove {
            scores.remove(&ip);
        }
    }

    /// Get a list of banned ips
    pub async fn get_banned_ips(&self) -> HashSet<IpAddr> {
        self.client_health_scores
            .read()
            .await
            .iter()
            .filter(|(_ip, score)| score > &&BAN_SCORE_THRESHOLD)
            .map(|(ip, _score)| *ip)
            .collect()
    }

    /// Flush database (save changes to disk)
    pub async fn flush(&self) {
        self.node_db.flush_async().await.unwrap();
    }

    /// Flush database sync (save changes to disk)
    pub fn flush_sync(&self) {
        self.node_db.flush().unwrap();
    }
}
