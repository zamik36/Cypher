//! Presentation shapes shared by every frontend (Tauri and WebAssembly):
//! hex ids, millisecond timestamps, one channel name per event.

use cypher_types::{FileId, MsgId, PeerId};
use serde::Serialize;

use crate::api::{Content, Event, FailReason, MediaKind, MessageStatus, StoredMessage};
use crate::store::ContactInfo;

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
    /// The message history on its way to (`outgoing`) or from another
    /// device of this identity: how far along, from 0 to 1, and why it
    /// stopped if it failed.
    History {
        file_id: String,
        outgoing: bool,
        progress: f64,
        failed: Option<String>,
    },
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
        Event::NoRoom { this, devices } => ("no_room", no_room(*this, devices)),
        Event::DeviceLinked { device, name } => ("device_linked", device_info(*device, name)),
        Event::LinkFailed { reason } => ("link_failed", UiPayload::Text(format!("{reason:?}"))),
        Event::LinkedHere => ("linked_here", UiPayload::None),
        Event::ContactsChanged => ("contacts_changed", UiPayload::None),
        Event::ServerKeyChanged { server } => {
            ("server_key_changed", UiPayload::Text(server.clone()))
        }
        Event::Warning {
            reason: FailReason::UpdateRequired,
        } => ("update_required", UiPayload::None),
        Event::Onion { up } => ("anonymity_level", anonymity(*up)),
        Event::PeerAdded { peer, .. } => ("peer_connected", UiPayload::Text(peer.to_hex())),
        Event::PeerProfile { peer, name } => ("peer_profile", profile(peer, name.clone())),
        Event::ContactRequest { peer } => ("contact_request", UiPayload::Text(peer.to_hex())),
        // Outgoing ones too: another device of this identity sent them.
        Event::Message(m) => ("message", UiPayload::Message(message(m))),
        Event::MessageStatus { msg_id, status } => ("message_status", status_of(msg_id, *status)),
        Event::TransferOffered { .. } => ("file_offered", offer(e)),
        Event::TransferProgress {
            file_id,
            bytes,
            total,
        } => ("file_progress", progress(file_id, *bytes, *total)),
        Event::HistoryProgress { .. } | Event::HistoryDone { .. } | Event::HistoryFailed { .. } => {
            history_event(e)
        }
        Event::TransferComplete { file_id } => ("file_complete", UiPayload::Text(file_id.to_hex())),
        Event::TransferFailed { file_id, reason } => ("file_failed", failed(file_id, *reason)),
        Event::JoinFailed { reason, .. } | Event::Warning { reason } => {
            ("error", UiPayload::Text(format!("{reason:?}")))
        }
        Event::PushKey { .. } | Event::PushRegistered | Event::PushUnavailable => push(e),
        Event::LinkCreated { .. } | Event::Bootstrap { .. } => return None,
    })
}

/// Push setup: the server's key (hex) to subscribe with, then its answer.
fn offer(e: &Event) -> UiPayload {
    let Event::TransferOffered {
        peer,
        file_id,
        name,
        size,
        mime,
    } = e
    else {
        return UiPayload::None;
    };
    UiPayload::Offer {
        from: peer.to_hex(),
        file_id: file_id.to_hex(),
        name: name.clone(),
        size: *size,
        mime: mime.clone(),
    }
}

/// The history's events: how far along it is, done, or why it stopped.
fn history_event(e: &Event) -> (&'static str, UiPayload) {
    if let Event::HistoryProgress {
        file_id,
        outgoing,
        bytes,
        total,
    } = e
    {
        let progress = ratio(*bytes, *total);
        return (
            "history_progress",
            history(file_id, *outgoing, progress, None),
        );
    }
    if let Event::HistoryDone { file_id, outgoing } = e {
        return ("history_done", history(file_id, *outgoing, 1.0, None));
    }
    let Event::HistoryFailed {
        file_id,
        outgoing,
        reason,
    } = e
    else {
        return ("history_failed", UiPayload::None);
    };
    (
        "history_failed",
        history(file_id, *outgoing, 0.0, Some(*reason)),
    )
}

fn failed(file_id: &FileId, reason: FailReason) -> UiPayload {
    UiPayload::Failed {
        file_id: file_id.to_hex(),
        reason: format!("{reason:?}"),
    }
}

fn history(
    file_id: &FileId,
    outgoing: bool,
    progress: f64,
    failed: Option<FailReason>,
) -> UiPayload {
    UiPayload::History {
        file_id: file_id.to_hex(),
        outgoing,
        progress,
        failed: failed.map(|reason| format!("{reason:?}")),
    }
}

fn progress(file_id: &FileId, bytes: u64, total: u64) -> UiPayload {
    UiPayload::Progress {
        file_id: file_id.to_hex(),
        progress: ratio(bytes, total),
    }
}

/// The devices of a full list, whose names this device does not know.
fn no_room(this: u32, devices: &[u32]) -> UiPayload {
    let unnamed: Vec<(u32, String)> = devices.iter().map(|d| (*d, String::new())).collect();
    own_devices(this, &unnamed)
}

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

/// How many of a contact's latest messages the chat list counts unread ones
/// among; it shows "99+" anyway.
pub const UNREAD_WINDOW: usize = 100;

/// A contact as the chat list shows it.
#[derive(Debug, Clone, Serialize)]
pub struct UiConversation {
    pub peer_id: String,
    /// The name the user gave this contact.
    pub alias: Option<String>,
    /// The name they go by, as they last sent it.
    pub name: Option<String>,
    /// Wrote without one of our invites; not accepted yet.
    pub request: bool,
    pub blocked: bool,
    /// When the latest message was sent; 0 without one.
    pub last_message_at: u64,
    pub last: Option<UiMessage>,
    /// Incoming messages not yet read, among the latest [`UNREAD_WINDOW`].
    pub unread: usize,
}

/// The chat-list entry of `peer`, from its contact row and its latest
/// messages, newest first; only the first [`UNREAD_WINDOW`] count.
pub fn summary(peer: &PeerId, contact: ContactInfo, recent: &[StoredMessage]) -> UiConversation {
    let recent = recent.get(..UNREAD_WINDOW).unwrap_or(recent);
    let last = recent.first();
    UiConversation {
        peer_id: peer.to_hex(),
        alias: contact.alias,
        name: contact.name,
        request: contact.request,
        blocked: contact.blocked,
        last_message_at: last.map_or(0, |m| m.sent_at_ms),
        last: last.map(message),
        unread: recent
            .iter()
            .filter(|m| !m.outgoing && m.status != MessageStatus::Read)
            .count(),
    }
}

/// The chat list: the most recent conversation first, ties in the order
/// given.
pub fn chat_list(mut entries: Vec<UiConversation>) -> Vec<UiConversation> {
    entries.sort_by_key(|c| std::cmp::Reverse(c.last_message_at));
    entries
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
    fn history_events_map_to_channels() {
        let file_id = FileId([1; 16]);
        assert_eq!(
            ui(&Event::HistoryProgress {
                file_id,
                outgoing: true,
                bytes: 1,
                total: 4
            }),
            (
                "history_progress",
                json!({ "file_id": file_id.to_hex(), "outgoing": true, "progress": 0.25, "failed": null })
            )
        );
        assert_eq!(
            ui(&Event::HistoryDone {
                file_id,
                outgoing: false
            }),
            (
                "history_done",
                json!({ "file_id": file_id.to_hex(), "outgoing": false, "progress": 1.0, "failed": null })
            )
        );
        assert_eq!(
            ui(&Event::HistoryFailed {
                file_id,
                outgoing: true,
                reason: FailReason::Cancelled
            }),
            (
                "history_failed",
                json!({ "file_id": file_id.to_hex(), "outgoing": true, "progress": 0.0, "failed": "Cancelled" })
            )
        );
    }

    #[test]
    fn connection_events_map_to_channels() {
        assert_eq!(ui(&Event::Connected), ("connected", Value::Null));
        assert_eq!(ui(&Event::Disconnected), ("disconnected", Value::Null));
        assert_eq!(ui(&Event::Superseded), ("superseded", Value::Null));
        assert_eq!(ui(&Event::DeviceUnlinked), ("unlinked", Value::Null));
        assert_eq!(
            ui(&Event::NoRoom {
                this: 7,
                devices: vec![1, 3]
            }),
            (
                "no_room",
                json!({
                    "this": 7,
                    "devices": [{ "id": 1, "name": "" }, { "id": 3, "name": "" }]
                })
            )
        );
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
        // Sent from another device of this identity: the UI shows it.
        let outgoing = Event::Message(stored(
            true,
            Content::Text {
                text: String::new(),
                reply_to: None,
            },
        ));
        assert_eq!(
            event(&outgoing).map(|(channel, _)| channel),
            Some("message")
        );
        assert_eq!(
            event(&Event::ContactsChanged).map(|(channel, _)| channel),
            Some("contacts_changed")
        );
        for e in [
            Event::LinkCreated { link: "l".into() },
            Event::Bootstrap {
                relay_addr: String::new(),
            },
        ] {
            assert!(event(&e).is_none(), "{e:?}");
        }
    }

    /// `n` messages, newest first: every third one ours, every fourth read.
    fn recent(n: u64) -> Vec<StoredMessage> {
        (0..n)
            .rev()
            .map(|at| StoredMessage {
                sent_at_ms: at,
                status: if at % 4 == 0 {
                    MessageStatus::Read
                } else {
                    MessageStatus::Delivered
                },
                ..stored(
                    at % 3 == 0,
                    Content::Text {
                        text: at.to_string(),
                        reply_to: None,
                    },
                )
            })
            .collect()
    }

    fn unread_among(msgs: &[StoredMessage]) -> usize {
        msgs.iter()
            .filter(|m| !m.outgoing && m.status != MessageStatus::Read)
            .count()
    }

    #[test]
    fn a_conversation_shows_its_latest_message_and_counts_unread_ones_within_the_window() {
        let peer = PeerId([9; 32]);
        let contact = ContactInfo {
            alias: Some("Al".into()),
            name: Some("Alice".into()),
            request: true,
            blocked: false,
        };
        let msgs = recent(250);
        let entry = summary(&peer, contact.clone(), &msgs);
        assert_eq!(entry.peer_id, peer.to_hex());
        assert_eq!((entry.alias, entry.name), (contact.alias, contact.name));
        assert_eq!((entry.request, entry.blocked), (true, false));
        let newest = msgs.first().unwrap();
        assert_eq!(entry.last_message_at, newest.sent_at_ms);
        assert_eq!(
            entry.last.map(|m| m.text),
            Some(newest.sent_at_ms.to_string())
        );
        let window = &msgs[..UNREAD_WINDOW];
        assert_eq!(entry.unread, unread_among(window));
        assert!(
            entry.unread < unread_among(&msgs),
            "older ones are not counted"
        );

        let empty = summary(&peer, ContactInfo::default(), &[]);
        assert_eq!((empty.last_message_at, empty.unread), (0, 0));
        assert!(empty.last.is_none() && empty.alias.is_none());
    }

    #[test]
    fn the_chat_list_puts_the_latest_conversation_first() {
        let at =
            |peer: u8, n: u64| summary(&PeerId([peer; 32]), ContactInfo::default(), &recent(n));
        let list = chat_list(vec![at(1, 5), at(2, 0), at(3, 9), at(4, 5)]);
        let order: Vec<_> = list
            .iter()
            .map(|c| c.peer_id.get(..2).unwrap().to_owned())
            .collect();
        assert_eq!(order, ["03", "01", "04", "02"], "ties keep their order");
        let json = serde_json::to_value(list.last().unwrap()).unwrap();
        assert_eq!(
            json,
            json!({
                "peer_id": "02".repeat(32), "alias": null, "name": null, "request": false,
                "blocked": false, "last_message_at": 0, "last": null, "unread": 0
            }),
            "the fields both clients' chat lists read"
        );
    }
}
