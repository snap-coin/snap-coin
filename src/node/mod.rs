/// A struct that handles one connected peer and all of its connection logic, and message processing
pub mod peer;

/// The node struct
pub mod node;

/// A struct for serializing and deserializing p2p messages
pub mod message;

/// Stores all currently pending transactions, that are waiting to be mined
pub mod mempool;

/// Synchronize if a fork happens
mod sync;

/// Host a server for other peers to connect to
mod server;