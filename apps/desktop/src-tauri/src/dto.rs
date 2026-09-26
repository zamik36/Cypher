//! UI-facing shapes: hex ids, millisecond timestamps, flat JSON.

use cypher_client::Content;
use cypher_core::{Event, MediaKind, MessageStatus, StoredMessage};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
pub struct UiFile {
    pub file_id: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub kind: &'static str,
    pub duration_ms: Option<u32>,
}

#[derive(Serialize, Clone)]
pub struct UiMessage {
    pub msg_id: String,
    pub from: String,
    pub outgoing: bool,
    pub text: String,
    pub timestamp: u64,
    pub status: &'static str,
    pub file: Option<UiFile>,
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
            let (kind, duration_ms) = match kind {
                MediaKind::File => ("file", None),
                MediaKind::Voice { duration_ms, .. } => ("voice", Some(*duration_ms)),
                MediaKind::VideoNote { duration_ms, .. } => ("video_note", Some(*duration_ms)),
            };
            let file = UiFile {
                file_id: file_id.to_hex(),
                name: name.clone(),
                size: *size,
                mime: mime.clone(),
                kind,
                duration_ms,
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

fn reason(r: impl std::fmt::Debug) -> String {
    format!("{r:?}")
}

/// Mirrors core events to the webview.
pub fn emit(app: &AppHandle, event: &Event) {
    let _ = match event {
        Event::Connected => app.emit("cypher://connected", ()),
        Event::Disconnected => app.emit("cypher://disconnected", ()),
        Event::Superseded => app.emit("cypher://error", "session opened on another device"),
        Event::Onion { up } => app.emit(
            "cypher://anonymity_level",
            serde_json::json!({
                "level": u8::from(*up),
                "label": if *up { "Onion relay" } else { "Direct" },
                "description": if *up {
                    "Inbox traffic is routed through the onion relay."
                } else {
                    "Onion relay unavailable; inbox traffic waits or uses the session."
                },
            }),
        ),
        Event::PeerAdded { peer, .. } => app.emit("cypher://peer_connected", peer.to_hex()),
        Event::Message(m) if !m.outgoing => app.emit("cypher://message", message(m)),
        Event::MessageStatus { msg_id, status: s } => app.emit(
            "cypher://message_status",
            serde_json::json!({ "msg_id": msg_id.to_hex(), "status": status(*s) }),
        ),
        Event::TransferOffered {
            peer,
            file_id,
            name,
            size,
            mime,
        } => app.emit(
            "cypher://file_offered",
            serde_json::json!({
                "from": peer.to_hex(),
                "file_id": file_id.to_hex(),
                "name": name,
                "size": size,
                "mime": mime,
            }),
        ),
        Event::TransferProgress {
            file_id,
            bytes,
            total,
        } => app.emit(
            "cypher://file_progress",
            serde_json::json!({
                "file_id": file_id.to_hex(),
                "progress": if *total == 0 { 1.0 } else { *bytes as f64 / *total as f64 },
            }),
        ),
        Event::TransferComplete { file_id } => app.emit("cypher://file_complete", file_id.to_hex()),
        Event::TransferFailed { file_id, reason: r } => app.emit(
            "cypher://file_failed",
            serde_json::json!({ "file_id": file_id.to_hex(), "reason": reason(r) }),
        ),
        Event::JoinFailed { reason: r, .. } | Event::Warning { reason: r } => {
            app.emit("cypher://error", reason(r))
        }
        Event::Message(_) | Event::LinkCreated { .. } | Event::Bootstrap { .. } => Ok(()),
    };
}
