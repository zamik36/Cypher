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
pub enum JsCommand {
    CreateLink,
    JoinLink {
        link: String,
    },
    SendText {
        peer: String,
        text: String,
    },
    SendFile {
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
    },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsMediaKind {
    File,
    Voice,
    VideoNote,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("invalid peer id")]
    Peer,
    #[error("invalid file id")]
    File,
    #[error("invalid message id")]
    Message,
    #[error("invalid file size")]
    Size,
}

/// A core command plus the ids generated for it, returned to the UI.
#[derive(Debug)]
pub struct Prepared {
    pub command: Command,
    pub msg_id: Option<MsgId>,
    pub file_id: Option<FileId>,
}

impl JsCommand {
    pub fn prepare(self, rng: &mut impl CryptoRngCore) -> Result<Prepared, CommandError> {
        let plain = |command| Prepared {
            command,
            msg_id: None,
            file_id: None,
        };
        Ok(match self {
            Self::CreateLink => plain(Command::CreateLink),
            Self::JoinLink { link } => plain(Command::JoinLink { link }),
            Self::SendText { peer: p, text } => {
                let msg_id = MsgId::random(rng);
                Prepared {
                    command: Command::SendText {
                        peer: peer(&p)?,
                        msg_id,
                        text,
                        reply_to: None,
                    },
                    msg_id: Some(msg_id),
                    file_id: None,
                }
            }
            Self::SendFile {
                peer: p,
                name,
                mime,
                size,
                kind,
                duration_ms,
                waveform,
                poster,
                inline,
            } => {
                let (msg_id, file_id) = (MsgId::random(rng), FileId::random(rng));
                let kind = match kind {
                    JsMediaKind::File => MediaKind::File,
                    JsMediaKind::Voice => MediaKind::Voice {
                        duration_ms: duration_ms.unwrap_or(0),
                        waveform: waveform.unwrap_or_default(),
                    },
                    JsMediaKind::VideoNote => MediaKind::VideoNote {
                        duration_ms: duration_ms.unwrap_or(0),
                        poster: poster.unwrap_or_default(),
                    },
                };
                Prepared {
                    command: Command::SendFile {
                        peer: peer(&p)?,
                        msg_id,
                        file_id,
                        name,
                        mime,
                        size: file_size(size)?,
                        kind,
                        inline,
                    },
                    msg_id: Some(msg_id),
                    file_id: Some(file_id),
                }
            }
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
            Self::SetAnonymity { require_onion } => plain(Command::SetAnonymity { require_onion }),
            Self::RemovePeer { peer: p } => plain(Command::RemovePeer { peer: peer(&p)? }),
        })
    }
}

pub fn peer(hex: &str) -> Result<PeerId, CommandError> {
    PeerId::from_hex(hex).ok_or(CommandError::Peer)
}

pub fn file(hex: &str) -> Result<FileId, CommandError> {
    FileId::from_hex(hex).ok_or(CommandError::File)
}

/// JS numbers are doubles: accept only whole, non-negative, exact values.
fn file_size(size: f64) -> Result<u64, CommandError> {
    if size.is_finite() && size >= 0.0 && size.fract() == 0.0 && size <= MAX_SAFE_INTEGER {
        // Whole, non-negative and below 2^53: the conversion is exact.
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
    fn unknown_command_types_and_media_kinds_do_not_parse() {
        assert!(serde_json::from_value::<JsCommand>(json!({ "type": "format_disk" })).is_err());
        assert!(
            serde_json::from_value::<JsCommand>(json!({
                "type": "send_file", "peer": PEER, "name": "f", "mime": "x/y",
                "size": 1, "kind": "hologram"
            }))
            .is_err()
        );
    }
}
