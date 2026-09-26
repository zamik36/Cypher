use bytes::Bytes;

pub use cypher_types::{FileId, LinkId, PeerId};

/// File metadata for transfer offers.
#[derive(Clone, Debug)]
pub struct FileMeta {
    pub file_id: FileId,
    pub name: String,
    pub size: u64,
    pub chunk_count: u32,
    pub hash: Bytes,
    /// Whether chunks are zstd-compressed before encryption.
    pub compressed: bool,
}

/// Transfer chunk size (256 KB).
pub const CHUNK_SIZE: usize = 256 * 1024;

/// Default window size for flow control.
pub const DEFAULT_WINDOW_SIZE: usize = 16;

/// Heartbeat interval in seconds.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Max missed heartbeats before disconnect.
pub const MAX_MISSED_HEARTBEATS: u32 = 3;

/// Per-session message rate limit (tokens per second).
pub const MSG_RATE_LIMIT_PER_SEC: u32 = 100;

/// Per-session message rate limit burst size.
pub const MSG_RATE_LIMIT_BURST: u32 = 200;
