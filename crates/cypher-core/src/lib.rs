//! Sans-IO Cypher client core shared by the native client and the
//! WebAssembly build. It owns every protocol and cryptographic decision;
//! drivers only move bytes between the core, the network and storage.

// Adding a protocol variant must be handled at every match, never swallowed
// by a catch-all arm.
#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

/// Declares a fieldless enum whose variants the frontends know by name:
/// each variant's name (also its serde name), `name()` and `ALL`, every
/// variant in order, come from one list, so none can be left out of one.
macro_rules! named_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $enum:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $name:literal, )*
        }
    ) => {
        $(#[$meta])*
        $vis enum $enum {
            $( $(#[$vmeta])* #[serde(rename = $name)] $variant, )*
        }

        impl $enum {
            /// Every variant, in the order declared.
            pub const ALL: &'static [Self] = &[$( Self::$variant, )*];

            /// The name the frontends know this by (its serde name).
            pub const fn name(self) -> &'static str {
                match self {
                    $( Self::$variant => $name, )*
                }
            }
        }
    };
}

mod api;
mod core;
pub mod envelope;
mod errors;
pub mod fs_name;
pub mod history;
pub mod link;
mod media;
mod peer;
mod prekeys;
pub mod relay;
mod share;
mod store;
mod transfer;
pub mod ui;

pub use api::{
    Command, CommandId, Content, Effect, Event, FailReason, Input, MediaKind, MessageStatus, Sink,
    StoredMessage,
};
pub use core::{Core, Rows, Snapshot};
pub use errors::{CmdError, ErrorKind, IdentityKind};
pub use media::MediaKey;
pub use share::ShareLink;
pub use store::{
    ContactInfo, MESSAGE_KEY_LEN, MessageDeletion, Record, StoreOp, Table, Vault, message_deletion,
    message_id, message_key, message_window, peer_messages, session_key,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("invalid or oversized input")]
    Invalid,
    #[error("stored state is corrupt or was written with another key")]
    Storage,
    #[error("stored state was written by a newer version of the app; update it")]
    NewerStorage,
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
