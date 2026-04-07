//! Client-side message and ratchet state persistence.
//!
//! All data is encrypted at rest with a Storage Encryption Key (SEK) derived
//! from the identity seed. Messages >64 bytes are compressed with zstd before
//! encryption.

use cypher_common::{PeerId, Result};
use cypher_crypto::RatchetState;

/// Direction of a stored message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum Direction {
    Sent = 0,
    Received = 1,
}

/// A message retrieved from persistent storage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredMessage {
    pub id: u64,
    pub peer_id: Vec<u8>,
    pub direction: Direction,
    pub plaintext: Vec<u8>,
    pub timestamp: u64,
}

/// Summary of a conversation for the conversation list.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Conversation {
    pub peer_id: Vec<u8>,
    pub nickname: Option<String>,
    pub created_at: u64,
    pub last_message_at: u64,
    /// Peer's blind inbox ID for offline message delivery.
    #[serde(default)]
    pub inbox_id: Option<Vec<u8>>,
}

/// Abstraction over platform-specific message stores (SQLite, IndexedDB, etc.).
pub trait MessageStore: Send + Sync {
    fn save_message(
        &self,
        peer_id: &PeerId,
        direction: Direction,
        plaintext: &[u8],
        timestamp: u64,
    ) -> Result<u64>;

    fn load_messages(
        &self,
        peer_id: &PeerId,
        limit: u32,
        before_id: Option<u64>,
    ) -> Result<Vec<StoredMessage>>;

    fn save_ratchet_state(&self, peer_id: &PeerId, state: &RatchetState) -> Result<()>;
    fn load_ratchet_state(&self, peer_id: &PeerId) -> Result<Option<RatchetState>>;

    fn save_conversation(&self, peer_id: &PeerId, nickname: Option<&str>) -> Result<()>;
    fn list_conversations(&self) -> Result<Vec<Conversation>>;
    fn delete_conversation(&self, peer_id: &PeerId) -> Result<()>;

    /// Save a peer's blind inbox ID for offline message delivery.
    fn save_peer_inbox_id(&self, peer_id: &PeerId, inbox_id: &[u8]) -> Result<()>;
    /// Load a peer's blind inbox ID.
    fn load_peer_inbox_id(&self, peer_id: &PeerId) -> Result<Option<Vec<u8>>>;

    /// Delete all messages, conversations, and ratchet states. Reclaims disk space.
    fn clear_all(&self) -> Result<()>;
}

pub mod encryption;
pub mod sqlite;
