/// Encodes and Decodes P2P messages
pub mod message;

/// Handles peer creation, with a PeerHandle struct for handling peers
pub mod peer;

/// A trait that handles what a peer does on message, and on kill
pub mod peer_behavior;