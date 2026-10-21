//! WebAssembly binding of `cypher-core` for the browser client. The seed never
//! leaves WebAssembly memory; JS only moves bytes between the core, the
//! WebSocket, `IndexedDB` and OPFS.

mod command;
mod convert;
mod history;

use cypher_core::{Core, Effect, Input, Vault, message_key, message_window, peer_messages, ui};
use cypher_crypto::{IdentitySeed, identity_file};
use cypher_types::{DeviceId, MsgId, PeerId};
use js_sys::Array;
use rand::rngs::OsRng;
use serde_bytes::ByteBuf;
use wasm_bindgen::prelude::*;

use command::{JsCommand, Prepared, file, file_size, peer};
use convert::{effects_to_js, snapshot_from_js, to_js};
pub use history::{HistoryReader, HistoryWriter};

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// Renders `text` as an SVG QR code.
#[wasm_bindgen(js_name = qrSvg)]
pub fn qr_svg(text: &str) -> Result<String, JsError> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(js_err)?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(180, 180)
        .quiet_zone(true)
        .build())
}

/// 64-bar voice waveform from per-frame RMS values measured while recording.
#[wasm_bindgen(js_name = waveformFromRms)]
pub fn waveform_from_rms(frames: &[f32]) -> Vec<u8> {
    cypher_media::waveform::from_frame_rms(frames)
}

#[wasm_bindgen]
pub struct Identity {
    seed: IdentitySeed,
    device: DeviceId,
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
        Self::seal_device(
            IdentitySeed::generate(),
            DeviceId::FIRST,
            nickname,
            passphrase,
        )
    }

    pub fn import(
        mnemonic: &str,
        nickname: &str,
        passphrase: &str,
    ) -> Result<SealedIdentity, JsError> {
        let seed = IdentitySeed::from_mnemonic(mnemonic.trim())
            .map_err(|_| JsError::new("invalid recovery phrase"))?;
        // A new device of the profile: the ones it had may still be in use,
        // and two installs as one device would push each other out.
        Self::seal_device(seed, DeviceId::random(&mut OsRng), nickname, passphrase)
    }

    pub fn unlock(blob: &[u8], passphrase: &str) -> Result<Self, JsError> {
        let file = identity_file::open(blob, passphrase).map_err(js_err)?;
        Ok(Self {
            seed: file.seed,
            device: file.device,
            nickname: file.nickname,
        })
    }

    fn seal_device(
        seed: IdentitySeed,
        device: DeviceId,
        nickname: &str,
        passphrase: &str,
    ) -> Result<SealedIdentity, JsError> {
        let blob =
            identity_file::seal(&seed, device, nickname, passphrase, &mut OsRng).map_err(js_err)?;
        Ok(SealedIdentity {
            identity: Self {
                seed,
                device,
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

    /// The 60-digit number to compare with `peer_hex` out of band.
    #[wasm_bindgen(js_name = safetyNumber)]
    pub fn safety_number(&self, peer_hex: &str) -> Result<String, JsError> {
        let own = self.seed.derive_identity().peer_id();
        Ok(ui::safety_number(&own, &peer(peer_hex)?))
    }

    /// Recovery phrase; the UI must re-verify the passphrase (`unlock`) first.
    pub fn mnemonic(&self) -> String {
        self.seed.to_mnemonic()
    }
}

/// A new device waiting to be linked, driven by the worker like `Client`
/// over its own gateway connection.
#[wasm_bindgen]
pub struct Provision {
    inner: cypher_core::link::Provision,
}

#[wasm_bindgen]
impl Provision {
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str, now_ms: f64) -> Self {
        Self {
            inner: cypher_core::link::Provision::new(name, now(now_ms), &mut OsRng),
        }
    }

    /// What to show for a device of the identity to scan.
    pub fn offer(&self) -> String {
        self.inner.offer().to_text()
    }

    #[wasm_bindgen(unchecked_return_type = "Effect[]")]
    pub fn connected(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.step(Input::Connected, now_ms)
    }

    #[wasm_bindgen(unchecked_return_type = "Effect[]")]
    pub fn frame(&mut self, data: &[u8], now_ms: f64) -> Result<Array, JsError> {
        self.step(Input::Frame(bytes::Bytes::copy_from_slice(data)), now_ms)
    }

    #[wasm_bindgen(unchecked_return_type = "Effect[]")]
    pub fn tick(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.step(Input::Tick, now_ms)
    }

    /// The identity handed over (after `linked_here`), sealed under this
    /// device's own `passphrase`; the seed never reaches JavaScript.
    pub fn finish(&mut self, passphrase: &str) -> Result<SealedIdentity, JsError> {
        let linked = self
            .inner
            .take_linked()
            .ok_or_else(|| JsError::new("not linked yet"))?;
        Identity::seal_device(linked.seed, linked.device, &linked.nickname, passphrase)
    }

    fn step(&mut self, input: Input, now_ms: f64) -> Result<Array, JsError> {
        effects_to_js(&self.inner.handle(input, now(now_ms)))
    }
}

#[wasm_bindgen]
pub struct Client {
    core: Core<OsRng>,
    vault: Vault,
    startup: Vec<Effect>,
}

#[wasm_bindgen]
impl Client {
    /// Restores the core from `IndexedDB` rows: `[key, value]` byte pairs
    /// from each store the core keeps its state in.
    #[wasm_bindgen(constructor)]
    pub fn new(
        identity: &Identity,
        #[wasm_bindgen(unchecked_param_type = "StoredState")] state: &JsValue,
        now_ms: f64,
    ) -> Result<Self, JsError> {
        let snapshot = snapshot_from_js(state)?;
        let (core, startup) = Core::restore(
            &identity.seed,
            identity.device,
            &snapshot,
            now(now_ms),
            OsRng,
        )
        .map_err(js_err)?;
        Ok(Self {
            core,
            vault: Vault::new(identity.seed.derive_storage_key()),
            startup,
        })
    }

    #[wasm_bindgen(js_name = startupEffects, unchecked_return_type = "Effect[]")]
    pub fn startup_effects(&mut self) -> Result<Array, JsError> {
        effects_to_js(&std::mem::take(&mut self.startup))
    }

    #[wasm_bindgen(js_name = peerId)]
    pub fn peer_id(&self) -> String {
        self.core.peer_id().to_hex()
    }

    #[wasm_bindgen(unchecked_return_type = "Effect[]")]
    pub fn connected(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Connected, now_ms)
    }

    #[wasm_bindgen(unchecked_return_type = "Effect[]")]
    pub fn disconnected(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Disconnected, now_ms)
    }

    #[wasm_bindgen(unchecked_return_type = "Effect[]")]
    pub fn frame(&mut self, data: &[u8], now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Frame(bytes::Bytes::copy_from_slice(data)), now_ms)
    }

    #[wasm_bindgen(js_name = anonymousFrame, unchecked_return_type = "Effect[]")]
    pub fn anonymous_frame(&mut self, data: &[u8], now_ms: f64) -> Result<Array, JsError> {
        self.feed(
            Input::AnonymousFrame(bytes::Bytes::copy_from_slice(data)),
            now_ms,
        )
    }

    #[wasm_bindgen(js_name = anonymousChannel, unchecked_return_type = "Effect[]")]
    pub fn anonymous_channel(&mut self, up: bool, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::AnonymousChannel { up }, now_ms)
    }

    #[wasm_bindgen(unchecked_return_type = "Effect[]")]
    pub fn tick(&mut self, now_ms: f64) -> Result<Array, JsError> {
        self.feed(Input::Tick, now_ms)
    }

    /// `buf` holds `headroom` reserved bytes followed by the chunk.
    #[wasm_bindgen(js_name = chunkRead, unchecked_return_type = "Effect[]")]
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

    #[wasm_bindgen(js_name = chunkUnavailable, unchecked_return_type = "Effect[]")]
    pub fn chunk_unavailable(&mut self, file_id: &str, now_ms: f64) -> Result<Array, JsError> {
        let file_id = file(file_id)?;
        self.feed(Input::ChunkUnavailable { file_id }, now_ms)
    }

    /// The archive `build_history` asked for was written: its size, or
    /// nothing when it could not be.
    #[wasm_bindgen(js_name = historyBuilt, unchecked_return_type = "Effect[]")]
    pub fn history_built(
        &mut self,
        file_id: &str,
        size: Option<f64>,
        now_ms: f64,
    ) -> Result<Array, JsError> {
        let file_id = file(file_id)?;
        let size = size.map(file_size).transpose()?;
        self.feed(Input::HistoryBuilt { file_id, size }, now_ms)
    }

    #[wasm_bindgen(js_name = historyImported, unchecked_return_type = "Effect[]")]
    pub fn history_imported(
        &mut self,
        file_id: &str,
        ok: bool,
        now_ms: f64,
    ) -> Result<Array, JsError> {
        let file_id = file(file_id)?;
        self.feed(Input::HistoryImported { file_id, ok }, now_ms)
    }

    /// Runs a UI command.
    #[wasm_bindgen(unchecked_return_type = "CommandOutcome")]
    pub fn command(&mut self, cmd: JsValue, now_ms: f64) -> Result<JsValue, JsError> {
        let cmd: JsCommand = serde_wasm_bindgen::from_value(cmd).map_err(js_err)?;
        let Prepared {
            command,
            msg_id,
            file_id,
        } = cmd.prepare(&mut OsRng).map_err(js_err)?;
        let effects = self.feed(Input::Command(command), now_ms)?;
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

    /// Decrypts a stored message, applying a newer status row if present.
    #[wasm_bindgen(js_name = openMessage, unchecked_return_type = "unknown")]
    pub fn open_message(
        &self,
        key: &[u8],
        value: &[u8],
        status: Option<Vec<u8>>,
    ) -> Result<JsValue, JsError> {
        let msg = self
            .vault
            .open_message_with_status(key, value, status)
            .map_err(js_err)?;
        to_js(&ui::message(&msg))
    }

    /// The chat list: each contact's row from `peers` with, at the same
    /// index of `recent`, its latest messages and their status rows, newest
    /// first. A contact row that cannot be read still lists its contact,
    /// unnamed.
    #[wasm_bindgen(js_name = chatList, unchecked_return_type = "ChatEntry[]")]
    pub fn chat_list(
        &self,
        #[wasm_bindgen(unchecked_param_type = "Row[]")] peers: JsValue,
        #[wasm_bindgen(unchecked_param_type = "MessageRow[][]")] recent: JsValue,
    ) -> Result<JsValue, JsError> {
        let peers: Vec<(ByteBuf, ByteBuf)> =
            serde_wasm_bindgen::from_value(peers).map_err(js_err)?;
        let recent: Vec<Vec<MessageRow>> =
            serde_wasm_bindgen::from_value(recent).map_err(js_err)?;
        let entries = chat_entries(&self.vault, &peers, &recent).map_err(js_err)?;
        convert::to_js_with_nulls(&ui::chat_list(entries))
    }

    /// What deleting the message stored under `key` takes: the store
    /// operations, the note whose sealed copy goes too, and the file it
    /// names. A row that cannot be read still goes, with its status.
    #[wasm_bindgen(js_name = messageDeletion, unchecked_return_type = "MessageDeletion")]
    pub fn message_deletion(&self, key: &[u8], value: &[u8]) -> Result<JsValue, JsError> {
        convert::deletion_to_js(&deletion(&self.vault, key, value))
    }

    /// Decrypts a sealed voice or video note: `record` is its row from the
    /// `media` table, `sealed` the file from OPFS.
    #[wasm_bindgen(js_name = openMedia, unchecked_return_type = "OpenedMedia")]
    pub fn open_media(
        &self,
        file_id: &str,
        record: &[u8],
        sealed: &[u8],
    ) -> Result<JsValue, JsError> {
        let key = self
            .vault
            .open_media(&file(file_id)?, record)
            .map_err(js_err)?;
        let bytes = key.open_all(sealed).map_err(js_err)?;
        let out = js_sys::Object::new();
        js_sys::Reflect::set(&out, &"mime".into(), &key.mime.as_str().into())
            .map_err(|_| JsError::new("reflect"))?;
        js_sys::Reflect::set(&out, &"bytes".into(), &js_sys::Uint8Array::from(&bytes[..]))
            .map_err(|_| JsError::new("reflect"))?;
        Ok(out.into())
    }

    /// A reader for a received history archive, checking its rows against
    /// this profile's vault.
    #[wasm_bindgen(js_name = historyReader)]
    pub fn history_reader(&self) -> HistoryReader {
        HistoryReader::new(self.vault.clone())
    }

    /// How long the sealed copy of a note is, by its row from `media`; or
    /// nothing when the row is not one this profile sealed.
    #[wasm_bindgen(js_name = noteLen)]
    pub fn note_len(&self, file_id: &str, record: &[u8]) -> Option<f64> {
        let key = self.vault.open_media(&file(file_id).ok()?, record).ok()?;
        Some(convert::js_number(key.sealed_len()))
    }

    fn feed(&mut self, input: Input, now_ms: f64) -> Result<Array, JsError> {
        effects_to_js(&self.core.handle(input, now(now_ms)))
    }
}

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = r#"
/** A message row with the row of its latest status, when there is one. */
export type MessageRow = [Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer> | undefined];

/** A contact as the chat list shows it. */
export interface ChatEntry {
  peer_id: string;
  alias: string | null;
  name: string | null;
  request: boolean;
  blocked: boolean;
  last_message_at: number;
  last: unknown;
  unread: number;
}
"#;

/// A message row, its value and the row of its latest status.
type MessageRow = (ByteBuf, ByteBuf, Option<ByteBuf>);

#[derive(Debug, thiserror::Error)]
enum ChatListError {
    #[error("one list of messages per contact")]
    Mismatch,
    #[error("{0}")]
    Core(#[from] cypher_core::CoreError),
}

/// The chat-list entry of each contact in `peers`, whose latest messages
/// are at the same index of `recent`; each contact row is opened once.
fn chat_entries(
    vault: &Vault,
    peers: &[(ByteBuf, ByteBuf)],
    recent: &[Vec<MessageRow>],
) -> Result<Vec<ui::UiConversation>, ChatListError> {
    if peers.len() != recent.len() {
        return Err(ChatListError::Mismatch);
    }
    let mut entries = Vec::with_capacity(peers.len());
    for ((key, sealed), rows) in peers.iter().zip(recent) {
        let Some(peer) = PeerId::from_bytes(key) else {
            continue;
        };
        let contact = vault.open_contact(key, sealed).unwrap_or_default();
        let msgs = rows
            .iter()
            .map(|(key, value, status)| {
                vault.open_message_with_status(key, value, status.as_deref())
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.push(ui::summary(&peer, contact, &msgs));
    }
    Ok(entries)
}

/// What deleting the message row `key`, `value` takes, readable or not.
fn deletion(vault: &Vault, key: &[u8], value: &[u8]) -> cypher_core::MessageDeletion {
    let msg = vault.open_message(key, value).ok();
    cypher_core::message_deletion(key, msg.as_ref())
}

/// The key of the status row of the message stored under `key`.
#[wasm_bindgen(js_name = statusKey, unchecked_return_type = "Uint8Array<ArrayBuffer>")]
pub fn status_key(key: &[u8]) -> Vec<u8> {
    cypher_core::status_key(key).to_vec()
}

/// How many of a contact's latest messages `chatList` wants.
#[wasm_bindgen(js_name = unreadWindow)]
pub fn unread_window() -> usize {
    ui::UNREAD_WINDOW
}

/// `[from, to]` `IndexedDB` key bounds of a whole conversation, both
/// included: every message with the peer lies between them, whatever its date.
#[wasm_bindgen(js_name = conversationRange, unchecked_return_type = "[Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>]")]
pub fn conversation_range(peer_hex: &str) -> Result<Array, JsError> {
    let (first, last) = peer_messages(&peer(peer_hex)?);
    Ok(key_bounds(&first, &last))
}

/// `[from, to)` `IndexedDB` key bounds of the part of a conversation sent
/// before `before_ms`, oldest first.
#[wasm_bindgen(js_name = historyRange, unchecked_return_type = "[Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>]")]
pub fn history_range(peer_hex: &str, before_ms: f64) -> Result<Array, JsError> {
    let p = peer(peer_hex)?;
    let (first, _) = peer_messages(&p);
    let before = message_key(&p, now(before_ms), &MsgId([0; MsgId::LEN]));
    Ok(key_bounds(&first, &before))
}

/// `[from, to]` `IndexedDB` key bounds of the messages with a peer sent
/// within `window_ms` of `at_ms`, both included: where to look for one
/// message whose time is known only roughly.
#[wasm_bindgen(js_name = messageWindow, unchecked_return_type = "[Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>]")]
pub fn message_window_range(peer_hex: &str, at_ms: f64, window_ms: f64) -> Result<Array, JsError> {
    let (first, last) = message_window(&peer(peer_hex)?, now(at_ms), now(window_ms));
    Ok(key_bounds(&first, &last))
}

fn key_bounds(from: &[u8], to: &[u8]) -> Array {
    let bounds = Array::new();
    bounds.push(&js_sys::Uint8Array::from(from));
    bounds.push(&js_sys::Uint8Array::from(to));
    bounds
}

/// JS timestamps (ms, a double) as core time; invalid values become 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite and positive checked; the cast saturates above u64::MAX"
)]
fn now(ms: f64) -> u64 {
    if ms.is_finite() && ms > 0.0 {
        ms as u64
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use cypher_core::{Content, MessageStatus, StoredMessage, Table, message_key};
    use rand::rngs::OsRng;

    use super::*;

    fn row(vault: &Vault, peer: PeerId, at: u64, status: Option<MessageStatus>) -> MessageRow {
        let msg = StoredMessage {
            msg_id: MsgId([u8::try_from(at).unwrap(); 16]),
            peer,
            outgoing: false,
            sent_at_ms: at,
            status: MessageStatus::Delivered,
            content: Content::Text {
                text: String::new(),
                reply_to: None,
            },
        };
        let key = message_key(&peer, at, &msg.msg_id);
        let value = vault.seal(Table::Messages, &key, &msg, &mut OsRng);
        let status = status.map(|s| {
            ByteBuf::from(vault.seal(Table::MessageStatus, msg.msg_id.as_bytes(), &s, &mut OsRng))
        });
        (ByteBuf::from(key), ByteBuf::from(value), status)
    }

    #[test]
    fn the_chat_list_counts_what_the_status_rows_say() {
        let vault = Vault::new([1; 32]);
        let (a, b) = (PeerId([1; 32]), PeerId([2; 32]));
        // A contact row that cannot be read lists its contact, unnamed.
        let contact = |p: PeerId| (ByteBuf::from(p.to_vec()), ByteBuf::from(vec![0; 40]));
        let peers = [contact(a), contact(b)];
        let recent = [
            vec![
                row(&vault, a, 3, None),
                row(&vault, a, 2, Some(MessageStatus::Read)),
                row(&vault, a, 1, None),
            ],
            Vec::new(),
        ];
        let list = ui::chat_list(chat_entries(&vault, &peers, &recent).unwrap());
        let shown: Vec<_> = list
            .iter()
            .map(|c| {
                (
                    c.peer_id.clone(),
                    c.last_message_at,
                    c.unread,
                    c.alias.is_none(),
                )
            })
            .collect();
        assert_eq!(shown, [(a.to_hex(), 3, 2, true), (b.to_hex(), 0, 0, true)]);

        assert!(matches!(
            chat_entries(&vault, &peers, &recent[..1]),
            Err(ChatListError::Mismatch)
        ));
        let stranger = [vec![row(&Vault::new([2; 32]), a, 1, None)], Vec::new()];
        chat_entries(&vault, &peers, &stranger).unwrap_err();
    }

    /// The web client deletes a row it cannot read with its status, as the
    /// native one does, instead of giving up the whole conversation.
    #[test]
    fn a_row_that_cannot_be_read_is_deleted_with_its_status() {
        let vault = Vault::new([1; 32]);
        let key = message_key(&PeerId([1; 32]), 5, &MsgId([7; 16]));
        let d = deletion(&vault, &key, b"not sealed here");
        let tables: Vec<_> = d
            .ops
            .iter()
            .map(|op| match op {
                cypher_core::StoreOp::Delete { table, key } => (*table, key.clone()),
                cypher_core::StoreOp::Put { .. } => panic!("a deletion puts nothing"),
            })
            .collect();
        assert_eq!(
            tables,
            [(Table::Messages, key), (Table::MessageStatus, vec![7; 16])]
        );
        assert_eq!((d.note, d.file), (None, None));
        let (k, v, _) = row(&vault, PeerId([1; 32]), 3, None);
        assert_eq!(deletion(&vault, &k, &v).ops.len(), 2);
    }
}
