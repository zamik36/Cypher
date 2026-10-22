//! Why a command failed, in one shape for both frontends (the desktop's
//! commands and the web worker): a stable `kind`, with a `reason` for some,
//! that the UI words for the user, and an English `message` for logs and
//! for whatever the UI cannot word.

use std::fmt;

use serde::Serialize;

use crate::api::FailReason;

/// What went wrong, by a `snake_case` name the UI keys its wording on.
/// Serialized as `{"kind": "...", "reason"?: "..."}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum ErrorKind {
    /// No identity is unlocked.
    Locked,
    /// No client runs.
    NotConnected,
    /// A client is being started; the UI tries again once it is up.
    Connecting,
    /// The client stopped while the command waited on it.
    Stopped,
    /// No reply came in time.
    TimedOut,
    /// Replies were missed while the command waited, maybe its own.
    Lagged,
    /// Stopped at the user's or the app's request.
    Cancelled,
    /// An argument is malformed: an id, a header, a body, an address.
    Invalid,
    TooLarge,
    /// Not on this platform, or not of a kind the app takes.
    Unsupported,
    NotFound,
    /// A note still on its way.
    NotReady,
    /// Nobody linked this device in time.
    Expired,
    /// A received file is no longer where it was saved.
    NotSaved,
    /// Opening the file would run it.
    UnsafeType,
    /// Nothing on the device opens this kind of file.
    NoApp,
    /// No file of this id is offered.
    UnknownOffer,
    NoDownloads,
    /// Every variant of the file's name is taken.
    NameTaken,
    /// The dropped files were sent, or another drop replaced them.
    DropGone,
    AlreadyRecording,
    NotRecording,
    /// This device is not waiting to be linked.
    NotLinking,
    /// The core said why.
    Failed(FailReason),
    /// The identity file or the recovery phrase.
    Identity(IdentityKind),
    /// Anything else; the message says what.
    Internal,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Locked => "identity is locked",
            Self::NotConnected => "not connected",
            Self::Connecting => "connecting",
            Self::Stopped => "client stopped",
            Self::TimedOut => "timed out",
            Self::Lagged => "replies were missed",
            Self::Cancelled => "cancelled",
            Self::Invalid => "invalid input",
            Self::TooLarge => "too large",
            Self::Unsupported => "unsupported",
            Self::NotFound => "not found",
            Self::NotReady => "not downloaded yet",
            Self::Expired => "the offer expired",
            Self::NotSaved => "the file is no longer saved",
            Self::UnsafeType => "opening the file would run it",
            Self::NoApp => "no app opens this file",
            Self::UnknownOffer => "unknown offer",
            Self::NoDownloads => "no downloads folder",
            Self::NameTaken => "too many files with this name",
            Self::DropGone => "this drop is gone",
            Self::AlreadyRecording => "already recording",
            Self::NotRecording => "not recording",
            Self::NotLinking => "not waiting to be linked",
            Self::Failed(reason) => return write!(f, "failed: {reason:?}"),
            Self::Identity(kind) => return kind.fmt(f),
            Self::Internal => "internal error",
        };
        f.write_str(text)
    }
}

/// What is wrong with the identity file or the recovery phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    /// This device has no identity.
    Missing,
    /// This device already has one.
    Exists,
    WeakPassphrase,
    WrongPassphrase,
    Corrupt,
    InvalidMnemonic,
    NicknameTooLong,
}

impl fmt::Display for IdentityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "no identity",
            Self::Exists => "an identity already exists",
            Self::WeakPassphrase => "passphrase is too short",
            Self::WrongPassphrase => "wrong passphrase",
            Self::Corrupt => "identity file is corrupt",
            Self::InvalidMnemonic => "invalid recovery phrase",
            Self::NicknameTooLong => "nickname is too long",
        })
    }
}

/// A command's failure as the UI gets it:
/// `{"kind": "...", "reason"?: "...", "message": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct CmdError {
    #[serde(flatten)]
    pub kind: ErrorKind,
    pub message: String,
}

impl CmdError {
    /// `kind`, told by `message` rather than the kind's own words.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Anything the UI has no words for; `e` says what.
    pub fn internal(e: impl fmt::Display) -> Self {
        Self::new(ErrorKind::Internal, e.to_string())
    }
}

impl From<ErrorKind> for CmdError {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind, kind.to_string())
    }
}

impl From<FailReason> for CmdError {
    fn from(reason: FailReason) -> Self {
        ErrorKind::Failed(reason).into()
    }
}

impl From<IdentityKind> for CmdError {
    fn from(kind: IdentityKind) -> Self {
        ErrorKind::Identity(kind).into()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// Every kind with the name the UI keys on. Exhaustive: a new kind does
    /// not compile until it is named here (and in the UI's dictionary).
    fn ui_name(kind: ErrorKind) -> &'static str {
        match kind {
            ErrorKind::Locked => "locked",
            ErrorKind::NotConnected => "not_connected",
            ErrorKind::Connecting => "connecting",
            ErrorKind::Stopped => "stopped",
            ErrorKind::TimedOut => "timed_out",
            ErrorKind::Lagged => "lagged",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::Invalid => "invalid",
            ErrorKind::TooLarge => "too_large",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::NotFound => "not_found",
            ErrorKind::NotReady => "not_ready",
            ErrorKind::Expired => "expired",
            ErrorKind::NotSaved => "not_saved",
            ErrorKind::UnsafeType => "unsafe_type",
            ErrorKind::NoApp => "no_app",
            ErrorKind::UnknownOffer => "unknown_offer",
            ErrorKind::NoDownloads => "no_downloads",
            ErrorKind::NameTaken => "name_taken",
            ErrorKind::DropGone => "drop_gone",
            ErrorKind::AlreadyRecording => "already_recording",
            ErrorKind::NotRecording => "not_recording",
            ErrorKind::NotLinking => "not_linking",
            ErrorKind::Failed(_) => "failed",
            ErrorKind::Identity(_) => "identity",
            ErrorKind::Internal => "internal",
        }
    }

    const PLAIN: [ErrorKind; 24] = [
        ErrorKind::Locked,
        ErrorKind::NotConnected,
        ErrorKind::Connecting,
        ErrorKind::Stopped,
        ErrorKind::TimedOut,
        ErrorKind::Lagged,
        ErrorKind::Cancelled,
        ErrorKind::Invalid,
        ErrorKind::TooLarge,
        ErrorKind::Unsupported,
        ErrorKind::NotFound,
        ErrorKind::NotReady,
        ErrorKind::Expired,
        ErrorKind::NotSaved,
        ErrorKind::UnsafeType,
        ErrorKind::NoApp,
        ErrorKind::UnknownOffer,
        ErrorKind::NoDownloads,
        ErrorKind::NameTaken,
        ErrorKind::DropGone,
        ErrorKind::AlreadyRecording,
        ErrorKind::NotRecording,
        ErrorKind::NotLinking,
        ErrorKind::Internal,
    ];

    fn identity_name(kind: IdentityKind) -> &'static str {
        match kind {
            IdentityKind::Missing => "missing",
            IdentityKind::Exists => "exists",
            IdentityKind::WeakPassphrase => "weak_passphrase",
            IdentityKind::WrongPassphrase => "wrong_passphrase",
            IdentityKind::Corrupt => "corrupt",
            IdentityKind::InvalidMnemonic => "invalid_mnemonic",
            IdentityKind::NicknameTooLong => "nickname_too_long",
        }
    }

    const IDENTITY: [IdentityKind; 7] = [
        IdentityKind::Missing,
        IdentityKind::Exists,
        IdentityKind::WeakPassphrase,
        IdentityKind::WrongPassphrase,
        IdentityKind::Corrupt,
        IdentityKind::InvalidMnemonic,
        IdentityKind::NicknameTooLong,
    ];

    fn json_of(e: &CmdError) -> Value {
        serde_json::to_value(e).unwrap()
    }

    #[test]
    fn plain_kinds_carry_no_reason() {
        for kind in PLAIN {
            let e = CmdError::from(kind);
            assert_eq!(
                json_of(&e),
                json!({ "kind": ui_name(kind), "message": kind.to_string() })
            );
        }
    }

    #[test]
    fn failures_carry_the_core_reason_and_identity_failures_theirs() {
        for reason in FailReason::ALL {
            let e = CmdError::from(reason);
            let shape = json_of(&e);
            assert_eq!(shape["kind"], "failed");
            assert_eq!(shape["reason"], serde_json::to_value(reason).unwrap());
        }
        for kind in IDENTITY {
            let e = CmdError::from(kind);
            assert_eq!(
                json_of(&e),
                json!({ "kind": "identity", "reason": identity_name(kind), "message": kind.to_string() })
            );
        }
    }

    #[test]
    fn a_message_of_its_own_keeps_the_kind() {
        let e = CmdError::new(ErrorKind::Invalid, "invalid peer id");
        assert_eq!(
            json_of(&e),
            json!({ "kind": "invalid", "message": "invalid peer id" })
        );
        assert_eq!(e.to_string(), "invalid peer id");
        assert_eq!(CmdError::internal("disk full").kind, ErrorKind::Internal);
    }

    /// The TypeScript the UI types its dictionaries with names exactly the
    /// kinds, reasons and identity kinds serialized here.
    #[test]
    fn the_ui_types_name_every_kind() {
        let ts = include_str!("../../../packages/ui/src/errors.ts");
        let union = |name: &str| -> std::collections::BTreeSet<String> {
            let start = format!("export type {name} =");
            let body = ts.split(&start).nth(1).unwrap().split(';').next().unwrap();
            body.split('|')
                .map(|n| n.trim().trim_matches('"').to_owned())
                .filter(|n| !n.is_empty())
                .collect()
        };
        let kinds = PLAIN
            .into_iter()
            .chain([
                ErrorKind::Failed(FailReason::Timeout),
                ErrorKind::Identity(IdentityKind::Missing),
            ])
            .map(|k| ui_name(k).to_owned())
            .collect();
        assert_eq!(union("ErrorKind"), kinds);
        let reasons = FailReason::ALL
            .into_iter()
            .map(|r| {
                serde_json::to_value(r)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(union("FailReason"), reasons);
        let identity = IDENTITY
            .into_iter()
            .map(|k| identity_name(k).to_owned())
            .collect();
        assert_eq!(union("IdentityKind"), identity);
    }
}
