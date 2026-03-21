use std::{collections::HashMap, net::IpAddr};

use tokio::sync::RwLock;

/// Encodes and Decodes P2P messages
pub mod message;

/// Handles peer creation, with a PeerHandle struct for handling peers
pub mod peer;

/// A trait that handles what a peer does on message, and on kill
pub mod peer_behavior;

/// Stores pending transactions
pub mod mempool;

pub mod chain_events;

pub const BAN_SCORE_THRESHOLD: u8 = 10;
pub const PUNISHMENT: u8 = 4;
pub type ClientHealthScores = RwLock<HashMap<IpAddr, u8>>;