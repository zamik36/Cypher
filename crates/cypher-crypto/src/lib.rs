//! Cryptography of Cypher: identities, X3DH handshake, Double Ratchet,
//! sealed sender, onion requests and per-file chunk encryption.

// Counters here are nonces and message numbers: overflow must be handled
// explicitly, never wrap or panic by accident.
#![cfg_attr(not(test), deny(clippy::arithmetic_side_effects))]

pub mod aead;
pub mod chunk;
pub mod devices;
pub mod double_ratchet;
mod error;
pub mod fingerprint;
pub mod handshake;
pub mod identity;
pub mod identity_file;
mod kdf;
pub mod onion;
pub mod prekey;
mod reader;
pub mod sealed;

pub use chunk::{ChunkCipher, FileKey};
pub use devices::DeviceList;
pub use double_ratchet::{Header, Ratchet};
pub use error::CryptoError;
pub use handshake::InitHeader;
pub use identity::{IdentityKeyPair, IdentitySeed};
pub use prekey::{OneTimePreKey, PrekeyBundle, SignedPreKey};
