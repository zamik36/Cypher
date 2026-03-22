use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Ephemeral peer identity (32 bytes, derived from Ed25519 public key).
#[derive(Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct PeerId(pub [u8; 32]);

impl PeerId {
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(b);
            Some(Self(arr))
        } else {
            None
        }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({})", hex_short(&self.0))
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex_short(&self.0))
    }
}

/// Share link identifier (random, URL-safe).
#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinkId(pub String);

impl LinkId {
    pub fn generate() -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let bytes: [u8; 16] = rng.gen();
        Self(base32_encode(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Session identifier.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct SessionId(pub u64);

/// File identifier (random 16 bytes).
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct FileId(pub [u8; 16]);

impl FileId {
    pub fn generate() -> Self {
        use rand::Rng;
        Self(rand::thread_rng().gen())
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() == 16 {
            let mut arr = [0u8; 16];
            arr.copy_from_slice(b);
            Some(Self(arr))
        } else {
            None
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

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

fn hex_short(b: &[u8]) -> String {
    let hex: String = b.iter().take(4).map(|b| format!("{:02x}", b)).collect();
    format!("{}...", hex)
}

fn base32_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut result = String::new();
    let mut bits = 0u32;
    let mut num_bits = 0;
    for &byte in data {
        bits = (bits << 8) | byte as u32;
        num_bits += 8;
        while num_bits >= 5 {
            num_bits -= 5;
            let idx = ((bits >> num_bits) & 0x1F) as usize;
            result.push(ALPHABET[idx] as char);
        }
    }
    if num_bits > 0 {
        let idx = ((bits << (5 - num_bits)) & 0x1F) as usize;
        result.push(ALPHABET[idx] as char);
    }
    result
}
