//! Native Cypher client: a tokio driver around the sans-IO `cypher-core`,
//! with SQLite persistence, positional file IO and automatic reconnects.

mod driver;
mod files;
mod history;
pub mod identity;
mod link;
mod media;
mod net;
mod store;
#[cfg(feature = "tor")]
mod tor;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cypher_core::envelope::MAX_INLINE_LEN;
use cypher_core::ui::{self, UiConversation};
use cypher_core::{
    Command, Core, CoreError, Event, MediaKind, Snapshot, StoredMessage, Table, Vault,
    message_deletion, message_key, message_window, peer_messages,
};
use cypher_crypto::IdentitySeed;
use cypher_types::{DeviceId, FileId, MsgId, PeerId};
use rand::rngs::OsRng;
use tokio::sync::{mpsc, oneshot, watch};

use driver::{Driver, FileEntry, Parts, SavedFile, file_key, now_ms, saved_key};
use files::SourceStamp;
use store::{End, FILES_TABLE, MessageRow, SAVED_TABLE, Store};

pub use cypher_core::{Content, FailReason};
pub use identity::{IdentityStore, Unlocked};
pub use link::{Provisioning, provision};
pub use media::{MAX_RANGE_LEN, MediaSlice};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("state: {0}")]
    Core(#[from] CoreError),
    #[error("client stopped")]
    Closed,
    #[error("an identity already exists")]
    IdentityExists,
    #[error("passphrase is too short")]
    WeakPassphrase,
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("identity file is corrupt")]
    CorruptIdentity,
    #[error("invalid recovery phrase")]
    InvalidMnemonic,
    #[error("invalid input")]
    InvalidInput,
    #[error("not found")]
    NotFound,
    /// The bytes asked for are still on their way (a note being received).
    #[error("not downloaded yet")]
    NotReady,
    /// Nobody linked this device in time.
    #[error("the offer expired")]
    Expired,
    /// Tor was asked for, but this build cannot reach the relay through it.
    #[error("built without Tor support")]
    TorUnavailable,
    /// Tor was asked for, but the application installed no process-wide
    /// rustls `CryptoProvider` for Arti to use.
    #[error("Tor needs a process-wide rustls CryptoProvider; install one at startup")]
    NoCryptoProvider,
}

#[derive(Clone)]
pub struct Config {
    /// `host:port` of the gateway.
    pub gateway_addr: String,
    /// Trust roots for the gateway and relay (system roots or a dev pin).
    pub tls: Arc<rustls::ClientConfig>,
    pub data_dir: PathBuf,
    /// Never fall back to the identity-bearing session for inbox traffic.
    pub require_onion: bool,
    /// Reach the onion relay through Tor. Needs the `tor` feature: without
    /// it, [`Client::start`] fails with [`ClientError::TorUnavailable`].
    /// Arti uses the process-wide rustls `CryptoProvider`, which the binary
    /// installs in `main`; without one, [`Client::start`] fails with
    /// [`ClientError::NoCryptoProvider`].
    pub tor: Option<TorConfig>,
}

#[derive(Clone, Debug, Default)]
pub struct TorConfig {
    /// Bridge lines for networks that block Tor (vanilla, obfs4, webtunnel).
    pub bridges: Vec<String>,
    /// Pluggable-transport binary (lyrebird) serving obfs4 and webtunnel.
    pub transport_binary: Option<PathBuf>,
}

impl Config {
    /// Sealed copy of a voice or video note.
    pub fn media_path(&self, file_id: &FileId) -> PathBuf {
        self.data_dir
            .join("media")
            .join(format!("{}.bin", file_id.to_hex()))
    }

    /// Plaintext of an outgoing recording, deleted once its transfer ends.
    pub(crate) fn outgoing_path(&self, file_id: &FileId) -> PathBuf {
        self.data_dir
            .join("outgoing")
            .join(format!("{}.bin", file_id.to_hex()))
    }

    /// A message history archive, built for another device or received from
    /// one; deleted once handed over or taken in.
    pub(crate) fn history_path(&self, file_id: &FileId) -> PathBuf {
        self.data_dir
            .join("history")
            .join(format!("{}.bin", file_id.to_hex()))
    }
}

pub(crate) enum Request {
    Command(Command),
    /// Records where a transfer lives, then optionally runs a command.
    Track {
        file_id: FileId,
        entry: FileEntry,
        then: Option<Command>,
    },
    /// Records that a finished file now lives at `location`.
    Saved {
        file_id: FileId,
        location: String,
        done: oneshot::Sender<()>,
    },
    /// Connects again after the session was taken over by another device.
    Reconnect,
    Shutdown,
}

/// Messages deleted at a time when a conversation is forgotten, so a long
/// one never sits in memory whole.
const FORGET_PAGE: usize = 256;

/// How far around the UI's time a deleted message is looked for: the UI
/// stamps outgoing messages itself, a few milliseconds off the core.
const DELETE_WINDOW_MS: u64 = 60_000;

/// Whether a Tor config can be honoured: never by reaching the relay
/// directly instead.
fn check_tor(config: &Config) -> Result<(), ClientError> {
    if config.tor.is_none() {
        return Ok(());
    }
    if !cfg!(feature = "tor") {
        return Err(ClientError::TorUnavailable);
    }
    // Left unset, rustls would pick a provider by itself the first time Arti
    // asks; that choice belongs to the application.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        return Err(ClientError::NoCryptoProvider);
    }
    Ok(())
}

/// Store operations that delete the message stored under `key` (`msg`,
/// when readable): see [`message_deletion`]. Returns the media note whose
/// sealed copy must go too.
fn deletion_ops(
    key: &[u8],
    msg: Option<&StoredMessage>,
    ops: &mut Vec<store::Op>,
) -> Option<FileId> {
    let deletion = message_deletion(key, msg);
    ops.extend(deletion.ops.into_iter().map(store::Op::from));
    if let Some(file_id) = deletion.file {
        ops.push(store::Op::Delete(SAVED_TABLE, file_id.to_vec()));
    }
    deletion.note
}

/// A file about to be offered: its id, where it is, the name it goes by.
struct Pick {
    file_id: FileId,
    path: PathBuf,
    name: String,
}

/// Someone the user has a session with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub peer: PeerId,
    /// The name the user gave them on this device.
    pub alias: Option<String>,
    /// The name they go by, as they last sent it.
    pub name: Option<String>,
    /// Wrote without one of our invites; not accepted yet.
    pub request: bool,
    pub blocked: bool,
}

#[derive(Clone)]
pub struct Client {
    tx: mpsc::Sender<Request>,
    /// Closes once the driver has stopped.
    stopped: watch::Receiver<()>,
    store: Store,
    vault: Arc<Vault>,
    peer_id: PeerId,
    config: Config,
}

impl Client {
    /// Restores persisted state for `seed` and starts connecting. Refuses a
    /// Tor config the build cannot honour rather than reach the relay
    /// directly.
    pub async fn start(
        seed: &IdentitySeed,
        device: DeviceId,
        config: Config,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Event>), ClientError> {
        check_tor(&config)?;
        let peer_id = seed.derive_identity().peer_id();
        let hex = peer_id.to_hex();
        let dir = config.data_dir.clone();
        let db = dir.join(format!("state-{}.db", hex.get(..16).unwrap_or(&hex)));
        // Opening SQLite, reading the state and decrypting all of it block.
        let restoring = IdentitySeed(*seed.as_bytes());
        let (store, files, core, initial) = blocking(move || {
            std::fs::create_dir_all(&dir)?;
            let store = Store::open(&db)?;
            let scan = |table: Table| store.blocking_scan(table.into());
            let snapshot = Snapshot {
                meta: scan(Table::Meta)?,
                peers: scan(Table::Peers)?,
                sessions: scan(Table::Sessions)?,
                outbox: scan(Table::Outbox)?,
                transfers: scan(Table::Transfers)?,
            };
            let vault = Vault::new(restoring.derive_storage_key());
            let files = load_files(&store, &vault)?;
            let (core, initial) = Core::restore(&restoring, device, &snapshot, now_ms(), OsRng)?;
            Ok((store, files, core, initial))
        })
        .await?;
        let vault = Vault::new(seed.derive_storage_key());

        // Unbounded by design: the driver emits from synchronous code, at the
        // pace of the bounded network queue and the app's own requests.
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (req_tx, req_rx) = mpsc::channel(256);
        let stopped = Driver::spawn(
            Parts {
                core,
                store: store.clone(),
                config: config.clone(),
                files,
                vault: Vault::new(seed.derive_storage_key()),
            },
            events_tx,
            req_rx,
            initial,
        )?;

        let client = Self {
            tx: req_tx,
            stopped,
            store,
            vault: Arc::new(vault),
            peer_id,
            config,
        };
        let server = Command::SetServer {
            addr: client.config.gateway_addr.clone(),
        };
        client.command(server).await?;
        if client.config.require_onion {
            client
                .command(Command::SetAnonymity {
                    require_onion: true,
                })
                .await?;
        }
        Ok((client, events_rx))
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn media_path(&self, file_id: &FileId) -> PathBuf {
        self.config.media_path(file_id)
    }

    pub async fn command(&self, cmd: Command) -> Result<(), ClientError> {
        self.send(Request::Command(cmd)).await
    }

    /// Sends a text message, optionally answering `reply_to`.
    pub async fn send_text(
        &self,
        peer: PeerId,
        text: String,
        reply_to: Option<MsgId>,
    ) -> Result<MsgId, ClientError> {
        let msg_id = MsgId::random(&mut OsRng);
        self.command(Command::SendText {
            peer,
            msg_id,
            text,
            reply_to,
        })
        .await?;
        Ok(msg_id)
    }

    /// Sends a file or media message. Small media travels inline.
    pub async fn send_file(
        &self,
        peer: PeerId,
        path: &Path,
        mime: &str,
        kind: MediaKind,
    ) -> Result<(MsgId, FileId), ClientError> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or(ClientError::InvalidInput)?;
        let file = Pick {
            file_id: FileId::random(&mut OsRng),
            path: path.to_owned(),
            name,
        };
        self.offer(peer, file, mime, kind).await
    }

    /// Sends a copy the app made only for sending (such as an Android
    /// `content://` pick) as `name`. The client takes the copy over and
    /// deletes it when the transfer ends.
    pub async fn send_staged_file(
        &self,
        peer: PeerId,
        staged: &Path,
        name: &str,
        mime: &str,
    ) -> Result<(MsgId, FileId), ClientError> {
        let file_id = FileId::random(&mut OsRng);
        let path = self.config.outgoing_path(&file_id);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        if tokio::fs::rename(staged, &path).await.is_err() {
            // Another file system: copy, then drop the original.
            tokio::fs::copy(staged, &path).await?;
            tokio::fs::remove_file(staged).await?;
        }
        let file = Pick {
            file_id,
            path,
            name: name.to_owned(),
        };
        self.offer(peer, file, mime, MediaKind::File).await
    }

    async fn offer(
        &self,
        peer: PeerId,
        Pick {
            file_id,
            path,
            name,
        }: Pick,
        mime: &str,
        kind: MediaKind,
    ) -> Result<(MsgId, FileId), ClientError> {
        let source = SourceStamp::of(&tokio::fs::metadata(&path).await?)?;
        let size = source.len();
        let inline = if kind.is_media() && size <= MAX_INLINE_LEN as u64 {
            Some(tokio::fs::read(&path).await?)
        } else {
            None
        };
        let msg_id = MsgId::random(&mut OsRng);
        let cmd = Command::SendFile {
            peer,
            msg_id,
            file_id,
            name,
            mime: mime.to_owned(),
            size,
            kind,
            inline,
        };
        self.send(Request::Track {
            file_id,
            entry: FileEntry {
                path,
                source: Some(source),
            },
            then: Some(cmd),
        })
        .await?;
        Ok((msg_id, file_id))
    }

    /// Sends a recording held in memory (voice or video note). Small notes
    /// travel inline; larger ones are staged in a temporary file.
    pub async fn send_media(
        &self,
        peer: PeerId,
        data: Vec<u8>,
        name: &str,
        mime: &str,
        kind: MediaKind,
    ) -> Result<(MsgId, FileId), ClientError> {
        if !kind.is_media() || data.is_empty() {
            return Err(ClientError::InvalidInput);
        }
        let (msg_id, file_id) = (MsgId::random(&mut OsRng), FileId::random(&mut OsRng));
        let size = data.len() as u64;
        let mut cmd = Command::SendFile {
            peer,
            msg_id,
            file_id,
            name: name.to_owned(),
            mime: mime.to_owned(),
            size,
            kind,
            inline: None,
        };
        if data.len() <= MAX_INLINE_LEN {
            if let Command::SendFile { inline, .. } = &mut cmd {
                *inline = Some(data);
            }
            self.command(cmd).await?;
            return Ok((msg_id, file_id));
        }
        let path = self.config.outgoing_path(&file_id);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(&path, data).await?;
        let source = SourceStamp::of(&tokio::fs::metadata(&path).await?)?;
        self.send(Request::Track {
            file_id,
            entry: FileEntry {
                path,
                source: Some(source),
            },
            then: Some(cmd),
        })
        .await?;
        Ok((msg_id, file_id))
    }

    /// Plaintext bytes `start..=end` of a stored voice or video note,
    /// decrypting only the chunks that cover the range.
    pub async fn media_range(
        &self,
        file_id: FileId,
        start: u64,
        end: Option<u64>,
    ) -> Result<MediaSlice, ClientError> {
        let raw = self
            .store
            .get(Table::Media.into(), file_id.to_vec())
            .await?
            .ok_or(ClientError::NotFound)?;
        let key = self.vault.open_media(&file_id, &raw)?;
        let path = self.config.media_path(&file_id);
        tokio::task::spawn_blocking(move || media::read_range(&path, &key, start, end))
            .await
            .map_err(|_| ClientError::Closed)?
    }

    /// Accepts an offered file, storing it at `dest`.
    pub async fn accept_file(&self, file_id: FileId, dest: PathBuf) -> Result<(), ClientError> {
        self.send(Request::Track {
            file_id,
            entry: FileEntry {
                path: dest,
                source: None,
            },
            then: Some(Command::AcceptFile { file_id }),
        })
        .await
    }

    /// Where a finished file is on this device: a path, or a URI the
    /// platform gave it. `None` for files never received or sent from here.
    pub async fn saved_file(&self, file_id: FileId) -> Result<Option<String>, ClientError> {
        let Some(sealed) = self.store.get(SAVED_TABLE, file_id.to_vec()).await? else {
            return Ok(None);
        };
        Ok(self
            .vault
            .open::<SavedFile>(Table::Meta, &saved_key(&file_id), &sealed)
            .ok()
            .map(|saved| saved.location))
    }

    /// Records that a finished file moved, e.g. into shared storage.
    pub async fn move_saved_file(
        &self,
        file_id: FileId,
        location: String,
    ) -> Result<(), ClientError> {
        let (done, saved) = oneshot::channel();
        self.send(Request::Saved {
            file_id,
            location,
            done,
        })
        .await?;
        saved.await.map_err(|_| ClientError::Closed)
    }

    /// Conversation history with `peer`, newest first, sent before `before_ms`.
    pub async fn history(
        &self,
        peer: PeerId,
        before_ms: Option<u64>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, ClientError> {
        let (from, last) = peer_messages(&peer);
        let to = before_ms.map_or(End::Through(last), |before| {
            End::Before(message_key(&peer, before, &MsgId([0; MsgId::LEN])))
        });
        let (store, vault) = (self.store.clone(), Arc::clone(&self.vault));
        blocking(move || {
            let rows = store.blocking_messages(vec![(from, to)], limit)?;
            open_messages(&vault, rows.first().map_or(&[], Vec::as_slice))
        })
        .await
    }

    /// Peers with an established session.
    /// Everyone with a session, with the name the user gave them. A row that
    /// cannot be read still lists its contact, unnamed.
    pub async fn contacts(&self) -> Result<Vec<Contact>, ClientError> {
        Ok(self
            .store
            .scan(Table::Peers.into())
            .await?
            .into_iter()
            .filter_map(|(key, sealed)| {
                let peer = PeerId::from_bytes(&key)?;
                let info = self.vault.open_contact(&key, &sealed).unwrap_or_default();
                Some(Contact {
                    peer,
                    alias: info.alias,
                    name: info.name,
                    request: info.request,
                    blocked: info.blocked,
                })
            })
            .collect())
    }

    /// Every conversation as the chat list shows it, the most recent first.
    /// A contact row that cannot be read still lists its contact, unnamed.
    pub async fn conversations(&self) -> Result<Vec<UiConversation>, ClientError> {
        let (store, vault) = (self.store.clone(), Arc::clone(&self.vault));
        blocking(move || {
            let contacts: Vec<_> = store
                .blocking_scan(Table::Peers.into())?
                .into_iter()
                .filter_map(|(key, sealed)| {
                    let peer = PeerId::from_bytes(&key)?;
                    Some((peer, vault.open_contact(&key, &sealed).unwrap_or_default()))
                })
                .collect();
            let spans = contacts
                .iter()
                .map(|(peer, _)| {
                    let (first, last) = peer_messages(peer);
                    (first, End::Through(last))
                })
                .collect();
            let recent = store.blocking_messages(spans, ui::UNREAD_WINDOW)?;
            let entries = contacts
                .into_iter()
                .zip(&recent)
                .map(|((peer, contact), rows)| {
                    Ok(ui::summary(&peer, contact, &open_messages(&vault, rows)?))
                })
                .collect::<Result<_, ClientError>>()?;
            Ok(ui::chat_list(entries))
        })
        .await
    }

    /// Sets the name this user goes by; contacts receive it encrypted.
    pub async fn set_profile_name(&self, name: Option<String>) -> Result<(), ClientError> {
        self.command(Command::SetProfileName { name }).await
    }

    /// Names a contact on this device; `None` or blank removes the name.
    pub async fn rename_contact(
        &self,
        peer: PeerId,
        alias: Option<String>,
    ) -> Result<(), ClientError> {
        self.command(Command::RenamePeer { peer, alias }).await
    }

    /// Ends the session with `peer` and deletes the conversation: messages,
    /// their statuses, stored voice and video notes and where files were
    /// saved. The saved files themselves stay where they are.
    pub async fn forget_peer(&self, peer: PeerId) -> Result<(), ClientError> {
        self.command(Command::RemovePeer { peer }).await?;
        let (store, vault) = (self.store.clone(), Arc::clone(&self.vault));
        let config = self.config.clone();
        blocking(move || {
            let (first, last) = peer_messages(&peer);
            loop {
                let page = store.blocking_range(
                    Table::Messages.into(),
                    first.clone(),
                    End::Through(last.clone()),
                    FORGET_PAGE,
                )?;
                if page.is_empty() {
                    return Ok(());
                }
                let mut ops = Vec::with_capacity(page.len() * 3);
                let mut notes = Vec::new();
                for (key, sealed) in &page {
                    let msg = vault.open_message(key, sealed).ok();
                    notes.extend(deletion_ops(key, msg.as_ref(), &mut ops));
                }
                store.blocking_apply(ops)?;
                remove_notes(&config, notes)?;
            }
        })
        .await
    }

    /// Deletes one message from this device: its row, status, media and
    /// where its file was saved (the file itself stays). One still going out
    /// is not sent any more; a file still moving is cancelled. `near_ms` is
    /// the message's time as the UI knows it, within a minute.
    pub async fn delete_message(
        &self,
        peer: PeerId,
        msg_id: MsgId,
        near_ms: u64,
    ) -> Result<(), ClientError> {
        let (from, to) = message_window(&peer, near_ms, DELETE_WINDOW_MS);
        let (key, sealed) = self
            .store
            .range_desc(Table::Messages.into(), from, End::Through(to), usize::MAX)
            .await?
            .into_iter()
            .find(|(key, _)| key.ends_with(msg_id.as_bytes()))
            .ok_or(ClientError::NotFound)?;
        let msg = self.vault.open_message(&key, &sealed)?;
        self.command(Command::DiscardOutgoing { msg_id }).await?;
        if let Content::File { file_id, .. } = &msg.content {
            self.command(Command::CancelTransfer { file_id: *file_id })
                .await?;
        }
        let mut ops = Vec::new();
        let note = deletion_ops(&key, Some(&msg), &mut ops);
        self.store.apply(ops).await?;
        let config = self.config.clone();
        blocking(move || remove_notes(&config, note)).await
    }

    /// Deletes every stored message and media note, and where files were
    /// saved; sessions, contacts and the saved files themselves are kept.
    pub async fn clear_history(&self) -> Result<(), ClientError> {
        self.store
            .apply(vec![
                store::Op::Clear(Table::Messages.into()),
                store::Op::Clear(Table::MessageStatus.into()),
                store::Op::Clear(Table::Media.into()),
                store::Op::Clear(SAVED_TABLE),
            ])
            .await?;
        match tokio::fs::remove_dir_all(self.config.data_dir.join("media")).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Takes the session back after another device took it over: connects
    /// again, which in turn disconnects that device.
    pub async fn reconnect(&self) -> Result<(), ClientError> {
        self.send(Request::Reconnect).await
    }

    /// Stops the client and waits until it has: what it persisted is on disk
    /// and it sends nothing more, so another client may open the same data.
    /// Two at once would each keep their own prekeys and overwrite the
    /// other's, leaving the server handing out keys this device lacks.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(Request::Shutdown).await;
        let mut stopped = self.stopped.clone();
        while stopped.changed().await.is_ok() {}
    }

    async fn send(&self, req: Request) -> Result<(), ClientError> {
        self.tx.send(req).await.map_err(|_| ClientError::Closed)
    }
}

/// Deletes the sealed copies of voice and video notes; one already gone is
/// fine.
fn remove_notes(
    config: &Config,
    notes: impl IntoIterator<Item = FileId>,
) -> Result<(), ClientError> {
    for file_id in notes {
        match std::fs::remove_file(config.media_path(&file_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// Opens message rows read with their status rows.
fn open_messages(vault: &Vault, rows: &[MessageRow]) -> Result<Vec<StoredMessage>, ClientError> {
    rows.iter()
        .map(|(key, value, status)| {
            Ok(vault.open_message_with_status(key, value, status.as_deref())?)
        })
        .collect()
}

/// Runs disk work or decryption in bulk on a thread that may block, off the
/// async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ClientError> + Send + 'static,
) -> Result<T, ClientError> {
    tokio::task::spawn_blocking(work).await.map_err(|e| {
        tracing::error!("blocking work did not finish: {e}");
        ClientError::Closed
    })?
}

fn load_files(store: &Store, vault: &Vault) -> Result<HashMap<FileId, FileEntry>, ClientError> {
    let mut files = HashMap::new();
    for (k, v) in store.blocking_scan(FILES_TABLE)? {
        let Some(file_id) = FileId::from_bytes(&k) else {
            continue;
        };
        // An unreadable entry only loses the ability to resume that one
        // transfer; data from a newer release must not be run on at all.
        match vault.open::<FileEntry>(Table::Meta, &file_key(&file_id), &v) {
            Ok(entry) => {
                files.insert(file_id, entry);
            }
            Err(CoreError::NewerStorage) => return Err(CoreError::NewerStorage.into()),
            Err(_) => {}
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use cypher_core::MessageStatus;

    use super::*;

    fn config(dir: &Path) -> Config {
        Config {
            gateway_addr: "localhost:1".into(),
            tls: cypher_tls::make_client_config(),
            data_dir: dir.to_owned(),
            require_onion: false,
            tor: None,
        }
    }

    fn text(peer: PeerId, sent_at_ms: u64, id: u8) -> StoredMessage {
        StoredMessage {
            msg_id: MsgId([id; MsgId::LEN]),
            peer,
            outgoing: false,
            sent_at_ms,
            status: MessageStatus::Delivered,
            content: Content::Text {
                text: format!("text {id}"),
                reply_to: None,
            },
        }
    }

    async fn put(client: &Client, msg: &StoredMessage) {
        let key = message_key(&msg.peer, msg.sent_at_ms, &msg.msg_id);
        let sealed = client.vault.seal(Table::Messages, &key, msg, &mut OsRng);
        client
            .store
            .apply(vec![store::Op::Put(Table::Messages.into(), key, sealed)])
            .await
            .unwrap();
    }

    fn file(peer: PeerId, sent_at_ms: u64, id: u8, kind: MediaKind) -> StoredMessage {
        StoredMessage {
            content: Content::File {
                file_id: FileId([id; FileId::LEN]),
                name: format!("file {id}"),
                mime: "audio/ogg".into(),
                size: 10,
                kind,
            },
            ..text(peer, sent_at_ms, id)
        }
    }

    async fn started(dir: &Path) -> Client {
        let seed = IdentitySeed::generate();
        let (client, _events) = Client::start(&seed, DeviceId::FIRST, config(dir))
            .await
            .unwrap();
        client
    }

    async fn put_status(client: &Client, msg_id: MsgId, status: MessageStatus) {
        let sealed =
            client
                .vault
                .seal(Table::MessageStatus, msg_id.as_bytes(), &status, &mut OsRng);
        let op = store::Op::Put(Table::MessageStatus.into(), msg_id.to_vec(), sealed);
        client.store.apply(vec![op]).await.unwrap();
    }

    /// A voice note's key row and its sealed copy on disk.
    async fn put_note(client: &Client, file_id: FileId) {
        let key = cypher_core::MediaKey::from_desc(&cypher_core::envelope::FileDesc {
            file_id,
            name: "voice".into(),
            mime: "audio/ogg".into(),
            size: 10,
            chunk_size: 64 * 1024,
            key: [1; 32],
            inline: None,
        });
        let sealed = client
            .vault
            .seal(Table::Media, file_id.as_bytes(), &key, &mut OsRng);
        let op = store::Op::Put(Table::Media.into(), file_id.to_vec(), sealed);
        client.store.apply(vec![op]).await.unwrap();
        let path = client.config.media_path(&file_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"sealed").unwrap();
    }

    /// Where a received file was saved, and the file itself.
    async fn put_saved(client: &Client, file_id: FileId, dir: &Path) -> PathBuf {
        let path = dir.join(format!("saved-{}", file_id.to_hex()));
        std::fs::write(&path, b"plain").unwrap();
        let location = path.to_str().unwrap().to_owned();
        let sealed = client.vault.seal(
            Table::Meta,
            &saved_key(&file_id),
            &SavedFile { location },
            &mut OsRng,
        );
        let op = store::Op::Put(SAVED_TABLE, file_id.to_vec(), sealed);
        client.store.apply(vec![op]).await.unwrap();
        path
    }

    async fn keys(client: &Client, table: store::StoreTable) -> Vec<Vec<u8>> {
        let rows = client.store.scan(table).await.unwrap();
        rows.into_iter().map(|(k, _)| k).collect()
    }

    /// History shows each message with the latest status stored for it.
    #[tokio::test]
    async fn history_shows_the_latest_status_of_each_message() {
        let dir = tempfile::tempdir().unwrap();
        let client = started(dir.path()).await;
        let peer = PeerId([4; 32]);
        for msg in [text(peer, 1, 1), text(peer, 2, 2)] {
            put(&client, &msg).await;
        }
        put_status(&client, MsgId([1; 16]), MessageStatus::Read).await;
        let shown: Vec<_> = client
            .history(peer, None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|m| (m.msg_id, m.status))
            .collect();
        assert_eq!(
            shown,
            [
                (MsgId([2; 16]), MessageStatus::Delivered),
                (MsgId([1; 16]), MessageStatus::Read)
            ]
        );
        client.shutdown().await;
    }

    /// Deleting a message or a conversation takes its rows, statuses and
    /// sealed notes, and where files were saved; the saved files stay.
    #[tokio::test]
    async fn deleting_takes_rows_statuses_and_notes_but_not_saved_files() {
        let dir = tempfile::tempdir().unwrap();
        let client = started(dir.path()).await;
        let (peer, other) = (PeerId([4; 32]), PeerId([5; 32]));
        let voice = MediaKind::Voice {
            duration_ms: 1,
            waveform: vec![1],
        };
        let msgs = [
            text(peer, 1_000, 1),
            text(peer, 2_000, 2),
            file(peer, 3_000, 3, voice),
            file(peer, 4_000, 4, MediaKind::File),
            text(other, 1_000, 5),
        ];
        for msg in &msgs {
            put(&client, msg).await;
            put_status(&client, msg.msg_id, MessageStatus::Read).await;
        }
        put_note(&client, FileId([3; 16])).await;
        let saved = put_saved(&client, FileId([4; 16]), dir.path()).await;

        client
            .delete_message(peer, MsgId([1; 16]), 1_000)
            .await
            .unwrap();
        let ids = |n: &[u8]| n.iter().map(|&n| vec![n; 16]).collect::<Vec<_>>();
        assert_eq!(
            keys(&client, Table::MessageStatus.into()).await,
            ids(&[2, 3, 4, 5])
        );
        assert_eq!(client.history(peer, None, 10).await.unwrap().len(), 3);

        client.forget_peer(peer).await.unwrap();
        assert!(client.history(peer, None, 10).await.unwrap().is_empty());
        assert_eq!(keys(&client, Table::MessageStatus.into()).await, ids(&[5]));
        assert!(keys(&client, Table::Media.into()).await.is_empty());
        assert!(!client.config.media_path(&FileId([3; 16])).exists());
        assert!(keys(&client, SAVED_TABLE).await.is_empty());
        assert!(saved.exists(), "a saved file stays where it is");
        assert_eq!(client.history(other, None, 10).await.unwrap().len(), 1);
        client.shutdown().await;
    }

    /// Asking for Tor never ends in a direct connection to the relay: a
    /// build without Tor, or a process that chose no rustls provider for
    /// Arti, refuses to start instead.
    #[tokio::test]
    async fn tor_is_honoured_or_the_client_does_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            tor: Some(TorConfig::default()),
            ..config(dir.path())
        };
        let seed = IdentitySeed::generate();
        let start = || Client::start(&seed, DeviceId::FIRST, config.clone());
        let nothing_opened = || {
            let entries = std::fs::read_dir(dir.path()).unwrap().count();
            assert_eq!(entries, 0, "nothing was opened");
        };
        if !cfg!(feature = "tor") {
            assert!(matches!(start().await, Err(ClientError::TorUnavailable)));
            return nothing_opened();
        }
        // Another test in this process may have chosen one already.
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            assert!(matches!(start().await, Err(ClientError::NoCryptoProvider)));
            nothing_opened();
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let (client, _events) = start().await.unwrap();
        client.shutdown().await;
    }

    /// Rows a contact dated as far ahead as the format allows, stored before
    /// the core clamped dates, are still shown and deleted with the rest.
    #[tokio::test]
    async fn a_conversation_dated_far_ahead_is_listed_and_forgotten_whole() {
        let dir = tempfile::tempdir().unwrap();
        let (client, _events) = Client::start(
            &IdentitySeed::generate(),
            DeviceId::FIRST,
            config(dir.path()),
        )
        .await
        .unwrap();
        let (peer, other) = (PeerId([4; 32]), PeerId([5; 32]));
        for msg in [
            text(peer, 1_000, 1),
            text(peer, u64::MAX, 2),
            text(peer, u64::MAX, u8::MAX),
            text(other, 1_000, 3),
        ] {
            put(&client, &msg).await;
        }

        let shown: Vec<_> = client
            .history(peer, None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.msg_id)
            .collect();
        assert_eq!(
            shown,
            [MsgId([u8::MAX; 16]), MsgId([2; 16]), MsgId([1; 16])],
            "newest first, the far-future ones included"
        );
        let older = client.history(peer, Some(u64::MAX), 10).await.unwrap();
        assert_eq!(older.len(), 1, "paging back starts before the date given");

        client.forget_peer(peer).await.unwrap();
        assert!(client.history(peer, None, 10).await.unwrap().is_empty());
        assert_eq!(client.history(other, None, 10).await.unwrap().len(), 1);
        client.shutdown().await;
    }

    /// A conversation longer than a page is forgotten whole, along with
    /// statuses and notes of every page and rows that cannot be read.
    #[tokio::test]
    async fn a_long_conversation_is_forgotten_whole() {
        let dir = tempfile::tempdir().unwrap();
        let client = started(dir.path()).await;
        let peer = PeerId([4; 32]);
        let total = FORGET_PAGE * 2 + 3;
        let mut ops = Vec::new();
        for n in 0..total {
            let mut msg = text(peer, u64::try_from(n).unwrap(), 0);
            msg.msg_id = MsgId(u128::try_from(n).unwrap().to_be_bytes());
            let key = message_key(&msg.peer, msg.sent_at_ms, &msg.msg_id);
            let sealed = client.vault.seal(Table::Messages, &key, &msg, &mut OsRng);
            ops.push(store::Op::Put(Table::Messages.into(), key, sealed));
        }
        let unreadable = message_key(&peer, u64::MAX, &MsgId([9; 16]));
        ops.push(store::Op::Put(
            Table::Messages.into(),
            unreadable,
            b"x".to_vec(),
        ));
        client.store.apply(ops).await.unwrap();
        let voice = MediaKind::Voice {
            duration_ms: 1,
            waveform: vec![1],
        };
        put(&client, &file(peer, 5, 7, voice)).await;
        put_note(&client, FileId([7; 16])).await;

        client.forget_peer(peer).await.unwrap();
        assert!(keys(&client, Table::Messages.into()).await.is_empty());
        assert!(keys(&client, Table::Media.into()).await.is_empty());
        assert!(!client.config.media_path(&FileId([7; 16])).exists());
        client.shutdown().await;
    }
}
