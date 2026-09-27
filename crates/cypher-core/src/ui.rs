//! Presentation shapes shared by every frontend (Tauri and WebAssembly):
//! hex ids, millisecond timestamps, one channel name per event.

use serde::Serialize;

use crate::api::{Content, Event, MediaKind, MessageStatus, StoredMessage};

#[derive(Debug, Clone, Serialize)]
pub struct UiFile {
    pub file_id: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub kind: &'static str,
    pub duration_ms: Option<u32>,
    /// Voice notes: bar heights 0..=255.
    pub waveform: Option<Vec<u8>>,
    /// Video notes: JPEG first frame shown before playback.
    pub poster: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiMessage {
    pub msg_id: String,
    pub from: String,
    pub outgoing: bool,
    pub text: String,
    pub timestamp: u64,
    pub status: &'static str,
    pub file: Option<UiFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum UiPayload {
    None,
    Text(String),
    Message(UiMessage),
    Status {
        msg_id: String,
        status: &'static str,
    },
    Offer {
        from: String,
        file_id: String,
        name: String,
        size: u64,
        mime: String,
    },
    Progress {
        file_id: String,
        progress: f64,
    },
    Failed {
        file_id: String,
        reason: String,
    },
    Anonymity {
        level: u8,
        label: &'static str,
        description: &'static str,
    },
}

pub fn status(s: MessageStatus) -> &'static str {
    match s {
        MessageStatus::Pending => "pending",
        MessageStatus::Sent => "sent",
        MessageStatus::Queued => "queued",
        MessageStatus::Delivered => "delivered",
        MessageStatus::Read => "read",
        MessageStatus::Failed => "failed",
    }
}

pub fn message(m: &StoredMessage) -> UiMessage {
    let (text, file) = match &m.content {
        Content::Text { text, .. } => (text.clone(), None),
        Content::File {
            file_id,
            name,
            mime,
            size,
            kind,
        } => {
            let (kind, duration_ms, waveform, poster) = match kind {
                MediaKind::File => ("file", None, None, None),
                MediaKind::Voice {
                    duration_ms,
                    waveform,
                } => ("voice", Some(*duration_ms), Some(waveform.clone()), None),
                MediaKind::VideoNote {
                    duration_ms,
                    poster,
                } => ("video_note", Some(*duration_ms), None, Some(poster.clone())),
            };
            let file = UiFile {
                file_id: file_id.to_hex(),
                name: name.clone(),
                size: *size,
                mime: mime.clone(),
                kind,
                duration_ms,
                waveform,
                poster,
            };
            (name.clone(), Some(file))
        }
    };
    UiMessage {
        msg_id: m.msg_id.to_hex(),
        from: m.peer.to_hex(),
        outgoing: m.outgoing,
        text,
        timestamp: m.sent_at_ms,
        status: status(m.status),
        file,
    }
}

/// Maps a core event to its UI channel; `None` for internal-only events.
pub fn event(e: &Event) -> Option<(&'static str, UiPayload)> {
    Some(match e {
        Event::Connected => ("connected", UiPayload::None),
        Event::Disconnected => ("disconnected", UiPayload::None),
        Event::Superseded => (
            "error",
            UiPayload::Text("session opened on another device".into()),
        ),
        Event::Onion { up } => ("anonymity_level", anonymity(*up)),
        Event::PeerAdded { peer, .. } => ("peer_connected", UiPayload::Text(peer.to_hex())),
        Event::Message(m) if !m.outgoing => ("message", UiPayload::Message(message(m))),
        Event::MessageStatus { msg_id, status: s } => (
            "message_status",
            UiPayload::Status {
                msg_id: msg_id.to_hex(),
                status: status(*s),
            },
        ),
        Event::TransferOffered {
            peer,
            file_id,
            name,
            size,
            mime,
        } => (
            "file_offered",
            UiPayload::Offer {
                from: peer.to_hex(),
                file_id: file_id.to_hex(),
                name: name.clone(),
                size: *size,
                mime: mime.clone(),
            },
        ),
        Event::TransferProgress {
            file_id,
            bytes,
            total,
        } => (
            "file_progress",
            UiPayload::Progress {
                file_id: file_id.to_hex(),
                progress: ratio(*bytes, *total),
            },
        ),
        Event::TransferComplete { file_id } => ("file_complete", UiPayload::Text(file_id.to_hex())),
        Event::TransferFailed { file_id, reason } => (
            "file_failed",
            UiPayload::Failed {
                file_id: file_id.to_hex(),
                reason: format!("{reason:?}"),
            },
        ),
        Event::JoinFailed { reason, .. } | Event::Warning { reason } => {
            ("error", UiPayload::Text(format!("{reason:?}")))
        }
        Event::Message(_) | Event::LinkCreated { .. } | Event::Bootstrap { .. } => return None,
    })
}

fn anonymity(up: bool) -> UiPayload {
    UiPayload::Anonymity {
        level: u8::from(up),
        label: if up { "Onion relay" } else { "Direct" },
        description: if up {
            "Inbox traffic is routed through the onion relay."
        } else {
            "Onion relay unavailable; inbox traffic waits or uses the session."
        },
    }
}

/// Transfer progress in `0..=1` for display.
#[expect(
    clippy::cast_precision_loss,
    reason = "display-only ratio; files are far below 2^52 bytes"
)]
fn ratio(bytes: u64, total: u64) -> f64 {
    if total == 0 {
        1.0
    } else {
        bytes as f64 / total as f64
    }
}
