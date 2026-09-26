//! WebAssembly binding of `cypher-core` for the browser client. The seed never
//! leaves WebAssembly memory; JS only moves bytes between the core, the
//! WebSocket, IndexedDB and OPFS.

mod convert;

use cypher_core::{
    Command, Core, Effect, Input, MediaKind, Snapshot, Table, Vault, message_key, ui,
};
use cypher_crypto::{IdentitySeed, identity_file};
use cypher_types::{FileId, MsgId, PeerId};
use js_sys::Array;
use rand::rngs::OsRng;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use convert::{effects_to_js, pairs_from_js, to_js};

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

#[wasm_bindgen]
pub struct Identity {
    seed: IdentitySeed,
    nickname: String,
}

/// A freshly created identity together with its encrypted form to persist.
#[wasm_bindgen]
pub struct SealedIdentity {
    identity: Identity,
    blob: Vec<u8>,
}

#[wasm_bindgen]
impl SealedIdentity {
    #[wasm_bindgen(getter)]
    pub fn blob(&self) -> Vec<u8> {
        self.blob.clone()
    }

    #[wasm_bindgen(js_name = intoIdentity)]
    pub fn into_identity(self) -> Identity {
        self.identity
    }
}

#[wasm_bindgen]
impl Identity {
    pub fn create(nickname: &str, passphrase: &str) -> Result<SealedIdentity, JsError> {
        Self::seal(IdentitySeed::generate(), nickname, passphrase)
    }

    pub fn import(
        mnemonic: &str,
        nickname: &str,
        passphrase: &str,
    ) -> Result<SealedIdentity, JsError> {
        let seed = IdentitySeed::from_mnemonic(mnemonic.trim())
            .map_err(|_| JsError::new("invalid recovery phrase"))?;
        Self::seal(seed, nickname, passphrase)
    }

    pub fn unlock(blob: &[u8], passphrase: &str) -> Result<Identity, JsError> {
        let (seed, nickname) = identity_file::open(blob, passphrase).map_err(js_err)?;
        Ok(Self { seed, nickname })
    }

    fn seal(
        seed: IdentitySeed,
        nickname: &str,
        passphrase: &str,
    ) -> Result<SealedIdentity, JsError> {
        let blob = identity_file::seal(&seed, nickname, passphrase, &mut OsRng).map_err(js_err)?;
        Ok(SealedIdentity {
            identity: Self {
                seed,
                nickname: nickname.to_owned(),
            },
            blob,
        })
    }

    #[wasm_bindgen(js_name = peerId)]
    pub fn peer_id(&self) -> String {
        self.seed.derive_identity().peer_id().to_hex()
    }

    #[wasm_bindgen(getter)]
    pub fn nickname(&self) -> String {
        self.nickname.clone()
    }

    /// Recovery phrase; the UI must re-verify the passphrase (`unlock`) first.
    pub fn mnemonic(&self) -> String {
        self.seed.to_mnemonic()
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JsCommand {
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
        kind: String,
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

#[wasm_bindgen]
pub struct Client {
    core: Core<OsRng>,
    vault: Vault,
    startup: Vec<Effect>,
}

fn peer(hex: &str) -> Result<PeerId, JsError> {
    PeerId::from_hex(hex).ok_or_else(|| JsError::new("invalid peer id"))
}

fn file(hex: &str) -> Result<FileId, JsError> {
    FileId::from_hex(hex).ok_or_else(|| JsError::new("invalid file id"))
}

#[wasm_bindgen]
impl Client {
    /// Restores the core from IndexedDB rows: each argument is an array of
    /// `[key, value]` byte pairs from the matching table.
    #[wasm_bindgen(constructor)]
    pub fn new(
        identity: &Identity,
        meta: Array,
        peers: Array,
        outbox: Array,
        transfers: Array,
        now_ms: f64,
    ) -> Result<Client, JsError> {
        let snapshot = Snapshot {
            meta: pairs_from_js(&meta)?,
            peers: pairs_from_js(&peers)?,
            outbox: pairs_from_js(&outbox)?,
            transfers: pairs_from_js(&transfers)?,
        };
        let (core, startup) =
            Core::restore(&identity.seed, snapshot, now(now_ms), OsRng).map_err(js_err)?;
        Ok(Self {
            core,
            vault: Vault::new(identity.seed.derive_storage_key()),
            startup,
        })
    }

    #[wasm_bindgen(js_name = startupEffects)]
    pub fn startup_effects(&mut self) -> Result<Array, JsError> {
        effects_to_js(std::mem::take(&mut self.startup))
    }

    #[wasm_bindgen(js_name = peerId)]
    pub fn peer_id(&self) -> String {
        self.core.peer_id().to_hex()
    }

    pub fn connected(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Connected, now_ms)
    }

    pub fn disconnected(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Disconnected, now_ms)
    }

    pub fn frame(&mut self, data: &[u8], now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Frame(bytes::Bytes::copy_from_slice(data)), now_ms)
    }

    #[wasm_bindgen(js_name = anonymousFrame)]
    pub fn anonymous_frame(&mut self, data: &[u8], now_ms: f64) -> Result<Array, JsError> {
        self.feed(
            Input::AnonymousFrame(bytes::Bytes::copy_from_slice(data)),
            now_ms,
        )
    }

    #[wasm_bindgen(js_name = anonymousChannel)]
    pub fn anonymous_channel(&mut self, up: bool, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::AnonymousChannel { up }, now_ms)
    }

    pub fn tick(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Tick, now_ms)
    }

    /// `buf` holds `headroom` reserved bytes followed by the chunk.
    #[wasm_bindgen(js_name = chunkRead)]
    pub fn chunk_read(
        &mut self,
        file_id: &str,
        index: u32,
        buf: Vec<u8>,
        now_ms: f64,
    ) -> Result<Array, JsError> {
        let file_id = file(file_id)?;
        self.feed(
            Input::ChunkRead {
                file_id,
                index,
                buf,
            },
            now_ms,
        )
    }

    #[wasm_bindgen(js_name = chunkUnavailable)]
    pub fn chunk_unavailable(&mut self, file_id: &str, now_ms: f64) -> Result<Array, JsError> {
        let file_id = file(file_id)?;
        self.feed(Input::ChunkUnavailable { file_id }, now_ms)
    }

    /// Runs a UI command. Returns `{ effects, msgId?, fileId? }`.
    pub fn command(&mut self, cmd: JsValue, now_ms: f64) -> Result<JsValue, JsError> {
        let cmd: JsCommand = serde_wasm_bindgen::from_value(cmd).map_err(js_err)?;
        let (mut msg_id, mut file_id) = (None, None);
        let cmd = match cmd {
            JsCommand::CreateLink => Command::CreateLink,
            JsCommand::JoinLink { link } => Command::JoinLink { link },
            JsCommand::SendText { peer: p, text } => {
                let id = MsgId::random(&mut OsRng);
                msg_id = Some(id);
                Command::SendText {
                    peer: peer(&p)?,
                    msg_id: id,
                    text,
                    reply_to: None,
                }
            }
            JsCommand::SendFile {
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
                let (m, f) = (MsgId::random(&mut OsRng), FileId::random(&mut OsRng));
                (msg_id, file_id) = (Some(m), Some(f));
                let kind = match kind.as_str() {
                    "voice" => MediaKind::Voice {
                        duration_ms: duration_ms.unwrap_or(0),
                        waveform: waveform.unwrap_or_default(),
                    },
                    "video_note" => MediaKind::VideoNote {
                        duration_ms: duration_ms.unwrap_or(0),
                        poster: poster.unwrap_or_default(),
                    },
                    _ => MediaKind::File,
                };
                Command::SendFile {
                    peer: peer(&p)?,
                    msg_id: m,
                    file_id: f,
                    name,
                    mime,
                    size: size as u64,
                    kind,
                    inline,
                }
            }
            JsCommand::AcceptFile { file_id: f } => Command::AcceptFile { file_id: file(&f)? },
            JsCommand::CancelTransfer { file_id: f } => {
                Command::CancelTransfer { file_id: file(&f)? }
            }
            JsCommand::MarkRead { peer: p, ids } => Command::MarkRead {
                peer: peer(&p)?,
                ids: ids.iter().filter_map(|h| MsgId::from_hex(h)).collect(),
            },
            JsCommand::FetchInbox => Command::FetchInbox,
            JsCommand::SetAnonymity { require_onion } => Command::SetAnonymity { require_onion },
            JsCommand::RemovePeer { peer: p } => Command::RemovePeer { peer: peer(&p)? },
        };
        let effects = self.feed(Input::Command(cmd), now_ms)?;
        let out = js_sys::Object::new();
        js_sys::Reflect::set(&out, &"effects".into(), &effects)
            .map_err(|_| JsError::new("reflect"))?;
        if let Some(id) = msg_id {
            js_sys::Reflect::set(&out, &"msgId".into(), &id.to_hex().into())
                .map_err(|_| JsError::new("reflect"))?;
        }
        if let Some(id) = file_id {
            js_sys::Reflect::set(&out, &"fileId".into(), &id.to_hex().into())
                .map_err(|_| JsError::new("reflect"))?;
        }
        Ok(out.into())
    }

    /// `[from, to)` IndexedDB key bounds of a conversation, oldest first.
    #[wasm_bindgen(js_name = historyRange)]
    pub fn history_range(&self, peer_hex: &str, before_ms: Option<f64>) -> Result<Array, JsError> {
        let p = peer(peer_hex)?;
        let before = before_ms.map_or(u64::MAX, now);
        let bounds = Array::new();
        bounds.push(&js_sys::Uint8Array::from(
            &message_key(&p, 0, &MsgId([0; 16]))[..],
        ));
        bounds.push(&js_sys::Uint8Array::from(
            &message_key(&p, before, &MsgId([0; 16]))[..],
        ));
        Ok(bounds)
    }

    /// Decrypts a stored message, applying a newer status row if present.
    #[wasm_bindgen(js_name = openMessage)]
    pub fn open_message(
        &self,
        key: &[u8],
        value: &[u8],
        status: Option<Vec<u8>>,
    ) -> Result<JsValue, JsError> {
        let mut msg = self.vault.open_message(key, value).map_err(js_err)?;
        if let Some(raw) = status
            && let Ok(s) = self
                .vault
                .open(Table::MessageStatus, msg.msg_id.as_bytes(), &raw)
        {
            msg.status = s;
        }
        to_js(&ui::message(&msg))
    }

    fn feed(&mut self, input: Input, now_ms: f64) -> Result<Array, JsError> {
        effects_to_js(self.core.handle(input, now(now_ms)))
    }
}

fn now(ms: f64) -> u64 {
    if ms.is_finite() && ms > 0.0 {
        ms as u64
    } else {
        0
    }
}
