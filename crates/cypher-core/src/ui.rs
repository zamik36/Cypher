//! Presentation shapes shared by every frontend (Tauri and WebAssembly):
//! hex ids, millisecond timestamps, one channel name per event.

use cypher_types::{MsgId, PeerId};
use serde::Serialize;

use crate::api::{Content, Event, FailReason, MediaKind, MessageStatus, StoredMessage};

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
    /// The message this one answers.
    pub reply_to: Option<String>,
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
    /// 1 while inbox traffic goes through the onion relay, 0 otherwise.
    Anonymity {
        level: u8,
    },
    Profile {
        peer: String,
        name: Option<String>,
    },
    /// The identity's devices, and which one this is.
    Devices {
        this: u32,
        devices: Vec<UiDevice>,
    },
    Device(UiDevice),
}

/// One device of this identity; `name` is empty when unknown.
#[derive(Debug, Clone, Serialize)]
pub struct UiDevice {
    pub id: u32,
    pub name: String,
}

fn device_info(id: u32, name: &str) -> UiPayload {
    UiPayload::Device(UiDevice {
        id,
        name: name.to_owned(),
    })
}

fn own_devices(this: u32, devices: &[(u32, String)]) -> UiPayload {
    UiPayload::Devices {
        this,
        devices: devices
            .iter()
            .map(|(id, name)| UiDevice {
                id: *id,
                name: name.clone(),
            })
            .collect(),
    }
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
    let (text, file, reply_to) = match &m.content {
        Content::Text { text, reply_to } => (text.clone(), None, reply_to.map(MsgId::to_hex)),
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
            (name.clone(), Some(file), None)
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
        reply_to,
    }
}

fn status_of(msg_id: &MsgId, s: MessageStatus) -> UiPayload {
    UiPayload::Status {
        msg_id: msg_id.to_hex(),
        status: status(s),
    }
}

/// Maps a core event to its UI channel; `None` for internal-only events.
pub fn event(e: &Event) -> Option<(&'static str, UiPayload)> {
    Some(match e {
        Event::Connected => ("connected", UiPayload::None),
        Event::Disconnected => ("disconnected", UiPayload::None),
        // Both stop the client until the user acts, so they get their own
        // channels rather than a passing error.
        Event::Superseded => ("superseded", UiPayload::None),
        Event::DeviceUnlinked => ("unlinked", UiPayload::None),
        Event::OwnDevices { this, devices } => ("devices", own_devices(*this, devices)),
        Event::DeviceLinked { device, name } => ("device_linked", device_info(*device, name)),
        Event::LinkFailed { reason } => ("link_failed", UiPayload::Text(format!("{reason:?}"))),
        Event::LinkedHere => ("linked_here", UiPayload::None),
        Event::Warning {
            reason: FailReason::UpdateRequired,
        } => ("update_required", UiPayload::None),
        Event::Onion { up } => ("anonymity_level", anonymity(*up)),
        Event::PeerAdded { peer, .. } => ("peer_connected", UiPayload::Text(peer.to_hex())),
        Event::PeerProfile { peer, name } => ("peer_profile", profile(peer, name.clone())),
        Event::ContactRequest { peer } => ("contact_request", UiPayload::Text(peer.to_hex())),
        Event::Message(m) if !m.outgoing => ("message", UiPayload::Message(message(m))),
        Event::MessageStatus { msg_id, status } => ("message_status", status_of(msg_id, *status)),
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
        Event::PushKey { .. } | Event::PushRegistered | Event::PushUnavailable => push(e),
        Event::Message(_) | Event::LinkCreated { .. } | Event::Bootstrap { .. } => return None,
    })
}

/// Push setup: the server's key (hex) to subscribe with, then its answer.
fn push(e: &Event) -> (&'static str, UiPayload) {
    if let Event::PushKey { key } = e {
        return ("push_key", UiPayload::Text(hex(key)));
    }
    let state = if matches!(e, Event::PushRegistered) {
        "registered"
    } else {
        "unavailable"
    };
    ("push_state", UiPayload::Text(state.to_owned()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The 60-digit number `own` and `peer` compare out of band to verify that
/// no one sits between them; both sides compute the same digits.
pub fn safety_number(own: &PeerId, peer: &PeerId) -> String {
    cypher_crypto::fingerprint::safety_number(own.as_bytes(), peer.as_bytes())
}

fn profile(peer: &PeerId, name: Option<String>) -> UiPayload {
    UiPayload::Profile {
        peer: peer.to_hex(),
        name,
    }
}

fn anonymity(up: bool) -> UiPayload {
    UiPayload::Anonymity {
        level: u8::from(up),
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

#[cfg(test)]
mod tests {
    use cypher_types::{FileId, MsgId, PeerId};
    use serde_json::{Value, json};

    use super::*;
    use crate::api::FailReason;

    fn ui(e: &Event) -> (&'static str, Value) {
        let (channel, payload) = event(e).unwrap();
        (channel, serde_json::to_value(payload).unwrap())
    }

    fn stored(outgoing: bool, content: Content) -> StoredMessage {
        StoredMessage {
            msg_id: MsgId([1; 16]),
            peer: PeerId([2; 32]),
            outgoing,
            sent_at_ms: 1_700,
            status: MessageStatus::Delivered,
            content,
        }
    }

    fn file(kind: MediaKind) -> Content {
        Content::File {
            file_id: FileId([3; 16]),
            name: "note.webm".into(),
            mime: "audio/webm".into(),
            size: 42,
            kind,
        }
    }

    #[test]
    fn connection_events_map_to_channels() {
        assert_eq!(ui(&Event::Connected), ("connected", Value::Null));
        assert_eq!(ui(&Event::Disconnected), ("disconnected", Value::Null));
        assert_eq!(ui(&Event::Superseded), ("superseded", Value::Null));
        assert_eq!(ui(&Event::DeviceUnlinked), ("unlinked", Value::Null));
        assert_eq!(
            ui(&Event::Warning {
                reason: FailReason::UpdateRequired
            }),
            ("update_required", Value::Null)
        );
        assert_eq!(
            ui(&Event::Warning {
                reason: FailReason::StorageFailed
            }),
            ("error", json!("StorageFailed"))
        );
        assert_eq!(
            ui(&Event::Onion { up: true }),
            ("anonymity_level", json!({ "level": 1 }))
        );
        assert_eq!(
            ui(&Event::Onion { up: false }),
            ("anonymity_level", json!({ "level": 0 }))
        );
    }

    #[test]
    fn text_message_shape() {
        let text = Content::Text {
            text: "привет".into(),
            reply_to: Some(MsgId([9; 16])),
        };
        let (channel, value) = ui(&Event::Message(stored(false, text)));
        assert_eq!(channel, "message");
        assert_eq!(
            value,
            json!({
                "msg_id": MsgId([1; 16]).to_hex(),
                "from": PeerId([2; 32]).to_hex(),
                "outgoing": false,
                "text": "привет",
                "timestamp": 1_700,
                "status": "delivered",
                "file": null,
                "reply_to": MsgId([9; 16]).to_hex(),
            })
        );
    }

    #[test]
    fn media_kinds_carry_their_extras() {
        let voice = message(&stored(
            true,
            file(MediaKind::Voice {
                duration_ms: 900,
                waveform: vec![7; 3],
            }),
        ));
        let voice_file = voice.file.unwrap();
        assert_eq!(
            (voice.text.as_str(), voice_file.kind),
            ("note.webm", "voice")
        );
        assert_eq!(
            (
                voice_file.duration_ms,
                voice_file.waveform,
                voice_file.poster
            ),
            (Some(900), Some(vec![7; 3]), None)
        );

        let note = message(&stored(
            true,
            file(MediaKind::VideoNote {
                duration_ms: 4_000,
                poster: vec![0xFF],
            }),
        ))
        .file
        .unwrap();
        assert_eq!(
            (note.kind, note.duration_ms, note.waveform, note.poster),
            ("video_note", Some(4_000), None, Some(vec![0xFF]))
        );

        let plain = message(&stored(true, file(MediaKind::File))).file.unwrap();
        assert_eq!(
            (plain.kind, plain.duration_ms, plain.size, plain.file_id),
            ("file", None, 42, FileId([3; 16]).to_hex())
        );
    }

    #[test]
    fn transfer_events_shape() {
        let file_id = FileId([3; 16]);
        let (channel, offer) = ui(&Event::TransferOffered {
            peer: PeerId([2; 32]),
            file_id,
            name: "a.bin".into(),
            size: 10,
            mime: "application/octet-stream".into(),
        });
        assert_eq!(channel, "file_offered");
        assert_eq!(
            (&offer["name"], &offer["size"], &offer["from"]),
            (
                &json!("a.bin"),
                &json!(10),
                &json!(PeerId([2; 32]).to_hex())
            )
        );

        let progress = |bytes, total| {
            ui(&Event::TransferProgress {
                file_id,
                bytes,
                total,
            })
            .1["progress"]
                .clone()
        };
        assert_eq!(progress(1, 4), json!(0.25));
        assert_eq!(progress(0, 0), json!(1.0), "empty files are complete");

        assert_eq!(
            ui(&Event::TransferComplete { file_id }),
            ("file_complete", json!(file_id.to_hex()))
        );
        let (channel, failed) = ui(&Event::TransferFailed {
            file_id,
            reason: FailReason::Cancelled,
        });
        assert_eq!(
            (channel, &failed["reason"]),
            ("file_failed", &json!("Cancelled"))
        );
    }

    #[test]
    fn statuses_and_warnings() {
        let statuses = [
            (MessageStatus::Pending, "pending"),
            (MessageStatus::Sent, "sent"),
            (MessageStatus::Queued, "queued"),
            (MessageStatus::Delivered, "delivered"),
            (MessageStatus::Read, "read"),
            (MessageStatus::Failed, "failed"),
        ];
        for (s, name) in statuses {
            let (channel, value) = ui(&Event::MessageStatus {
                msg_id: MsgId([1; 16]),
                status: s,
            });
            assert_eq!(
                (channel, &value["status"]),
                ("message_status", &json!(name))
            );
        }
        assert_eq!(
            ui(&Event::Warning {
                reason: FailReason::Timeout
            }),
            ("error", json!("Timeout"))
        );
        let join = Event::JoinFailed {
            link: "x".into(),
            reason: FailReason::InvalidLink,
        };
        assert_eq!(ui(&join), ("error", json!("InvalidLink")));
        let peer = PeerId([2; 32]);
        assert_eq!(
            ui(&Event::PeerAdded {
                peer,
                initiated_by_us: true
            }),
            ("peer_connected", json!(peer.to_hex()))
        );
    }

    #[test]
    fn internal_events_stay_internal() {
        let outgoing = Event::Message(stored(
            true,
            Content::Text {
                text: String::new(),
                reply_to: None,
            },
        ));
        for e in [
            outgoing,
            Event::LinkCreated { link: "l".into() },
            Event::Bootstrap {
                relay_addr: String::new(),
                onion_key: [0; 32],
            },
        ] {
            assert!(event(&e).is_none(), "{e:?}");
        }
    }
}
