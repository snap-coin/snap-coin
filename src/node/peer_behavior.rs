use std::sync::Arc;

use crate::node::{message::Message, peer::{PeerError, PeerHandle}};

pub type SharedPeerBehavior = Arc<dyn PeerBehavior + Send + Sync>;

#[async_trait::async_trait]
pub trait PeerBehavior {
    /// Handles what this peer does when it receives a message, and creates this peers response
    async fn on_message(&self, message: Message, peer: &PeerHandle) -> Result<Message, PeerError>;

    /// Handles what happens when this peer gets killed (apart from peer process' being killed)
    async fn on_kill(&self, peer: &PeerHandle);

    /// Return current blockchain height
    async fn get_height(&self) -> usize;
}