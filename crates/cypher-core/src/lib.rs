//! Sans-IO Cypher client core shared by the native client and the
//! WebAssembly build. It owns every protocol and cryptographic decision;
//! drivers only move bytes between the core, the network and storage.

// Adding a protocol variant must be handled at every match, never swallowed
// by a catch-all arm.
#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

mod api;
mod core;
pub mod envelope;
pub mod fs_name;
mod media;
mod peer;
mod prekeys;
pub mod relay;
mod store;
mod transfer;
pub mod ui;

pub use api::{
    Command, Content, Effect, Event, FailReason, Input, MediaKind, MessageStatus, StoredMessage,
};
pub use core::{Core, Rows, Snapshot};
pub use media::MediaKey;
pub use store::{StoreOp, Table, Vault, message_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("invalid or oversized input")]
    Invalid,
    #[error("stored state is corrupt or was written with another key")]
    Storage,
    #[error("cryptographic failure")]
    Crypto,
    #[error("concurrent session initiation resolved in favour of ours")]
    Conflict,
}

impl From<cypher_crypto::CryptoError> for CoreError {
    fn from(_: cypher_crypto::CryptoError) -> Self {
        Self::Crypto
    }
}
