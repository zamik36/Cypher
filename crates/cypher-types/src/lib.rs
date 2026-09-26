//! Platform-neutral primitives shared by every Cypher crate, including the
//! WebAssembly build: identifiers, the error type and protocol constants.

mod error;
mod id;

pub use error::{Error, Result};
pub use id::{FileId, LinkId, MsgId, PeerId};

/// Domain-separation prefix for the gateway session proof-of-possession.
pub const SESSION_AUTH_CONTEXT: &[u8] = b"cypher-session-auth-v2";

/// Upper bound for a single transport frame payload.
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

pub use hex;
