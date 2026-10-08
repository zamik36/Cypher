//! UI commands as the web client sends them (`{ type: "send_text", … }`)
//! and their validated mapping onto core commands. Pure, so it is tested
//! natively; the wasm-bindgen layer only converts `JsValue`s.

use cypher_core::{Command, MediaKind};
use cypher_types::{FileId, MsgId, PeerId};
use rand_core::CryptoRngCore;
use serde::Deserialize;

/// Largest integer a JS number represents exactly.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum JsCommand {
    CreateLink,
    JoinLink {
        link: String,
    },
    SendText {
        peer: String,
        text: String,
        #[serde(default)]
        reply_to: Option<String>,
    },
    DiscardOutgoing {
        msg_id: String,
    },
    SendFile(JsFile),
    AcceptFile {
        file_id: String,
    },
    CancelTransfer {
        file_id: String,
    },
    MarkRead {
        peer: String,
        ids: Vec<String>,
    },
    FetchInbox,
    SetAnonymity {
        require_onion: bool,
    },
    RemovePeer {
        peer: String,
    },
    RenamePeer {
        peer: String,
        alias: Option<String>,
    },
    SetProfileName {
        name: Option<String>,
    },
    AcceptContact {
        peer: String,
    },
    BlockPeer {
        peer: String,
    },
    UnblockPeer {
        peer: String,
    },
    EnablePush,
    /// The browser's push subscription: keys as raw bytes.
    RegisterPush {
        endpoint: String,
        #[serde(with = "serde_bytes")]
        p256dh: Vec<u8>,
        #[serde(with = "serde_bytes")]
        auth: Vec<u8>,
    },
    DisablePush,
    LinkDevice {
        offer: String,
    },
    UnlinkDevice {
        device: u32,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct JsFile {
    peer: String,
    name: String,
    mime: String,
    size: f64,
    kind: JsMediaKind,
    duration_ms: Option<u32>,
    waveform: Option<Vec<u8>>,
    #[serde(default, with = "serde_bytes")]
    poster: Option<Vec<u8>>,
    #[serde(default, with = "serde_bytes")]
    inline: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JsMediaKind {
    File,
    Voice,
    VideoNote,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CommandError {
    #[error("invalid peer id")]
    Peer,
    #[error("invalid file id")]
    File,
    #[error("invalid message id")]
    Message,
    #[error("invalid file size")]
    Size,
    #[error("invalid push subscription")]
    Push,
}

/// A core command plus the ids generated for it, returned to the UI.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub command: Command,
    pub msg_id: Option<MsgId>,
    pub file_id: Option<FileId>,
}

impl JsCommand {
    pub(crate) fn prepare(self, rng: &mut impl CryptoRngCore) -> Result<Prepared, CommandError> {
        let plain = |command| Prepared {
            command,
            msg_id: None,
            file_id: None,
        };
        Ok(match self {
            Self::CreateLink => plain(Command::CreateLink),
            Self::JoinLink { link } => plain(Command::JoinLink { link }),
            Self::SendText {
                peer: p,
                text,
                reply_to,
            } => {
                let msg_id = MsgId::random(rng);
                let reply_to = reply_to
                    .map(|h| MsgId::from_hex(&h).ok_or(CommandError::Message))
                    .transpose()?;
                Prepared {
                    command: Command::SendText {
                        peer: peer(&p)?,
                        msg_id,
                        text,
                        reply_to,
                    },
                    msg_id: Some(msg_id),
                    file_id: None,
                }
            }
            Self::SendFile(file) => file.prepare(rng)?,
            Self::AcceptFile { file_id } => plain(Command::AcceptFile {
                file_id: file(&file_id)?,
            }),
            Self::CancelTransfer { file_id } => plain(Command::CancelTransfer {
                file_id: file(&file_id)?,
            }),
            Self::MarkRead { peer: p, ids } => plain(Command::MarkRead {
                peer: peer(&p)?,
                ids: ids
                    .iter()
                    .map(|h| MsgId::from_hex(h).ok_or(CommandError::Message))
                    .collect::<Result<_, _>>()?,
            }),
            Self::FetchInbox => plain(Command::FetchInbox),
            Self::DiscardOutgoing { msg_id } => plain(Command::DiscardOutgoing {
                msg_id: MsgId::from_hex(&msg_id).ok_or(CommandError::Message)?,
            }),
            Self::SetAnonymity { require_onion } => plain(Command::SetAnonymity { require_onion }),
            Self::RemovePeer { peer: p } => plain(Command::RemovePeer { peer: peer(&p)? }),
            Self::RenamePeer { peer: p, alias } => plain(Command::RenamePeer {
                peer: peer(&p)?,
                alias,
            }),
            Self::SetProfileName { name } => plain(Command::SetProfileName { name }),
            Self::AcceptContact { peer: p } => plain(Command::AcceptContact { peer: peer(&p)? }),
            Self::BlockPeer { peer: p } => plain(Command::BlockPeer { peer: peer(&p)? }),
            Self::UnblockPeer { peer: p } => plain(Command::UnblockPeer { peer: peer(&p)? }),
            // Push wake-ups and this identity's other devices.
            other => plain(other.device_command()?),
        })
    }
}

impl JsCommand {
    /// Commands about this device: its push wake-ups and its siblings.
    fn device_command(self) -> Result<Command, CommandError> {
        Ok(match self {
            Self::EnablePush => Command::EnablePush,
            Self::RegisterPush {
                endpoint,
                p256dh,
                auth,
            } => Command::RegisterPush {
                endpoint,
                p256dh: p256dh.try_into().map_err(|_| CommandError::Push)?,
                auth: auth.try_into().map_err(|_| CommandError::Push)?,
            },
            Self::DisablePush => Command::DisablePush,
            Self::LinkDevice { offer } => Command::LinkDevice { offer },
            Self::UnlinkDevice { device } => Command::UnlinkDevice { device },
            _ => return Err(CommandError::Push),
        })
    }
}

impl JsFile {
    fn prepare(self, rng: &mut impl CryptoRngCore) -> Result<Prepared, CommandError> {
        let (msg_id, file_id) = (MsgId::random(rng), FileId::random(rng));
        let kind = match self.kind {
            JsMediaKind::File => MediaKind::File,
            JsMediaKind::Voice => MediaKind::Voice {
                duration_ms: self.duration_ms.unwrap_or(0),
                waveform: self.waveform.unwrap_or_default(),
            },
            JsMediaKind::VideoNote => MediaKind::VideoNote {
                duration_ms: self.duration_ms.unwrap_or(0),
                poster: self.poster.unwrap_or_default(),
            },
        };
        Ok(Prepared {
            command: Command::SendFile {
                peer: peer(&self.peer)?,
                msg_id,
                file_id,
                name: self.name,
                mime: self.mime,
                size: file_size(self.size)?,
                kind,
                inline: self.inline,
            },
            msg_id: Some(msg_id),
            file_id: Some(file_id),
        })
    }
}

pub(crate) fn peer(hex: &str) -> Result<PeerId, CommandError> {
    PeerId::from_hex(hex).ok_or(CommandError::Peer)
}

pub(crate) fn file(hex: &str) -> Result<FileId, CommandError> {
    FileId::from_hex(hex).ok_or(CommandError::File)
}

/// JS numbers are doubles: accept only whole, non-negative, exact values.
fn file_size(size: f64) -> Result<u64, CommandError> {
    if size.is_finite() && size >= 0.0 && size.fract() == 0.0 && size <= MAX_SAFE_INTEGER {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked above: whole, non-negative and below 2^53, so exact"
        )]
        let exact = size as u64;
        Ok(exact)
    } else {
        Err(CommandError::Size)
    }
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;
    use serde_json::json;

    use super::*;

    const PEER: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    fn prepare(value: serde_json::Value) -> Result<Prepared, CommandError> {
        serde_json::from_value::<JsCommand>(value)
            .expect("well-formed command")
            .prepare(&mut OsRng)
    }

    #[test]
    fn send_text_gets_a_fresh_message_id() {
        let p = prepare(json!({ "type": "send_text", "peer": PEER, "text": "hi" })).unwrap();
        let Command::SendText { msg_id, text, .. } = p.command else {
            panic!("expected SendText");
        };
        assert_eq!((Some(msg_id), text.as_str()), (p.msg_id, "hi"));
        assert!(p.file_id.is_none());
    }

    #[test]
    fn voice_notes_carry_duration_and_waveform() {
        let p = prepare(json!({
            "type": "send_file", "peer": PEER, "name": "voice.webm", "mime": "audio/webm",
            "size": 2048, "kind": "voice", "duration_ms": 1500, "waveform": [1, 2, 3]
        }))
        .unwrap();
        let Command::SendFile {
            kind,
            size,
            file_id,
            ..
        } = p.command
        else {
            panic!("expected SendFile");
        };
        assert_eq!(size, 2048);
        assert_eq!(Some(file_id), p.file_id);
        assert_eq!(
            kind,
            MediaKind::Voice {
                duration_ms: 1500,
                waveform: vec![1, 2, 3]
            }
        );
    }

    #[test]
    fn rejects_bad_ids_and_sizes() {
        let text = |peer: &str| json!({ "type": "send_text", "peer": peer, "text": "x" });
        assert_eq!(prepare(text("zz")).unwrap_err(), CommandError::Peer);
        assert_eq!(
            prepare(json!({ "type": "accept_file", "file_id": "00" })).unwrap_err(),
            CommandError::File
        );
        assert_eq!(
            prepare(json!({ "type": "mark_read", "peer": PEER, "ids": ["nope"] })).unwrap_err(),
            CommandError::Message
        );
        for size in [-1.0, 1.5, f64::MAX] {
            let file = json!({
                "type": "send_file", "peer": PEER, "name": "f", "mime": "x/y",
                "size": size, "kind": "file"
            });
            assert_eq!(prepare(file).unwrap_err(), CommandError::Size, "{size}");
        }
    }

    #[test]
    fn contacts_can_be_named_and_unnamed() {
        let rename = |alias: serde_json::Value| {
            prepare(json!({ "type": "rename_peer", "peer": PEER, "alias": alias }))
                .unwrap()
                .command
        };
        let Command::RenamePeer { alias, .. } = rename(json!("Anna")) else {
            panic!("not a rename");
        };
        assert_eq!(alias.as_deref(), Some("Anna"));
        let Command::RenamePeer { alias, .. } = rename(json!(null)) else {
            panic!("not a rename");
        };
        assert_eq!(alias, None);
        let bad = json!({ "type": "rename_peer", "peer": "zz", "alias": "x" });
        assert_eq!(prepare(bad).unwrap_err(), CommandError::Peer);
        let named = prepare(json!({ "type": "set_profile_name", "name": "Anna" })).unwrap();
        assert!(matches!(named.command, Command::SetProfileName { name: Some(n) } if n == "Anna"));
    }

    #[test]
    fn push_subscriptions_carry_whole_keys() {
        let register = |p256dh: usize, auth: usize| {
            prepare(json!({
                "type": "register_push", "endpoint": "https://ntfy.sh/up",
                "p256dh": vec![4u8; p256dh], "auth": vec![1u8; auth]
            }))
        };
        let Command::RegisterPush { endpoint, .. } = register(65, 16).unwrap().command else {
            panic!("not a registration");
        };
        assert_eq!(endpoint, "https://ntfy.sh/up");
        assert_eq!(register(64, 16).unwrap_err(), CommandError::Push);
        assert_eq!(register(65, 8).unwrap_err(), CommandError::Push);
        let enable = prepare(json!({ "type": "enable_push" })).unwrap();
        assert!(matches!(enable.command, Command::EnablePush));
        let disable = prepare(json!({ "type": "disable_push" })).unwrap();
        assert!(matches!(disable.command, Command::DisablePush));
    }

    #[test]
    fn devices_are_linked_and_unlinked() {
        let link = prepare(json!({ "type": "link_device", "offer": "cypher-device:00" })).unwrap();
        assert!(
            matches!(link.command, Command::LinkDevice { offer } if offer == "cypher-device:00")
        );
        let unlink = prepare(json!({ "type": "unlink_device", "device": 3 })).unwrap();
        assert!(matches!(
            unlink.command,
            Command::UnlinkDevice { device: 3 }
        ));
    }

    #[test]
    fn unknown_command_types_and_media_kinds_do_not_parse() {
        serde_json::from_value::<JsCommand>(json!({ "type": "format_disk" })).unwrap_err();
        serde_json::from_value::<JsCommand>(json!({
            "type": "send_file", "peer": PEER, "name": "f", "mime": "x/y",
            "size": 1, "kind": "hologram"
        }))
        .unwrap_err();
    }
}
