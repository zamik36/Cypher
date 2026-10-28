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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cypher_core::envelope::MAX_INLINE_LEN;
use cypher_core::ui::{self, UiConversation};
use cypher_core::{
    CmdError, Command, Core, CoreError, Effect, ErrorKind, Event, IdentityKind, MediaKind,
    Snapshot, StoredMessage, Table, Vault, message_deletion, message_id, message_key,
    message_window, peer_messages,
};
use cypher_crypto::IdentitySeed;
use cypher_types::{DeviceId, FileId, MsgId, PeerId};
use rand::rngs::OsRng;
use tokio::sync::{mpsc, oneshot, watch};

use driver::{Driver, FileEntry, Parts, SavedFile, file_key, now_ms, saved_key};
use files::SourceStamp;
use store::{End, FILES_TABLE, FORGET_TABLE, MessageRow, SAVED_TABLE, Store};

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
    /// The identity file or the recovery phrase.
    #[error("{0}")]
    Identity(IdentityKind),
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

/// The kind the UI words a client failure by; the message is the error's.
impl From<ClientError> for CmdError {
    fn from(e: ClientError) -> Self {
        let kind = match &e {
            ClientError::Io(_)
            | ClientError::Sqlite(_)
            | ClientError::Core(CoreError::Storage | CoreError::Crypto | CoreError::Conflict)
            | ClientError::NoCryptoProvider => ErrorKind::Internal,
            ClientError::Core(CoreError::Invalid) | ClientError::InvalidInput => ErrorKind::Invalid,
            ClientError::Core(CoreError::NewerStorage) => {
                ErrorKind::Failed(FailReason::UpdateRequired)
            }
            ClientError::Closed => ErrorKind::Stopped,
            ClientError::Identity(kind) => ErrorKind::Identity(*kind),
            ClientError::NotFound => ErrorKind::NotFound,
            ClientError::NotReady => ErrorKind::NotReady,
            ClientError::Expired => ErrorKind::Expired,
            ClientError::TorUnavailable => ErrorKind::Unsupported,
        };
        Self::new(kind, e.to_string())
    }
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

    /// Plaintext of an outgoing recording, or a copy made only for sending,
    /// deleted once its transfer ends.
    pub(crate) fn outgoing_path(&self, file_id: &FileId) -> PathBuf {
        outgoing_dir(&self.data_dir).join(format!("{}.bin", file_id.to_hex()))
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

/// A contact whose forgetting did not finish: a crash cut it short, or it
/// failed while the app ran on.
struct Forgetting {
    peer: PeerId,
    /// Set when `peer` is a contact (or a request) again: only messages up
    /// to this key, the newest when the forget began, are what it was
    /// about; empty when there were none.
    upto: Option<Vec<u8>>,
}

/// What a start reads from disk.
struct Restored {
    store: Store,
    files: HashMap<FileId, FileEntry>,
    /// Contacts whose forgetting did not finish.
    forgetting: Vec<Forgetting>,
    core: Core<OsRng>,
    initial: Vec<Effect>,
}

/// Opens the store at `db` (in `dir`), reads the state and restores the
/// core from it: all blocking on the disk or on decryption.
fn restore(
    seed: &IdentitySeed,
    device: DeviceId,
    dir: &Path,
    db: &Path,
) -> Result<Restored, ClientError> {
    std::fs::create_dir_all(dir)?;
    let store = Store::open(db)?;
    let scan = |table: Table| store.blocking_scan(table.into());
    let snapshot = Snapshot {
        meta: scan(Table::Meta)?,
        peers: scan(Table::Peers)?,
        sessions: scan(Table::Sessions)?,
        outbox: scan(Table::Outbox)?,
        transfers: scan(Table::Transfers)?,
    };
    let vault = Vault::new(seed.derive_storage_key());
    let file_rows = store.blocking_scan(FILES_TABLE)?;
    // Named by a row, even one unreadable now, or by a transfer of the core.
    let named = file_rows
        .iter()
        .chain(&snapshot.transfers)
        .filter_map(|(key, _)| FileId::from_bytes(key))
        .collect();
    sweep_outgoing(dir, &named);
    let files = load_files(file_rows, &vault)?;
    let forgetting = store
        .blocking_scan(FORGET_TABLE)?
        .into_iter()
        .filter_map(|(key, bound)| {
            let peer = PeerId::from_bytes(&key)?;
            let contact = snapshot.peers.iter().any(|(k, _)| *k == key);
            Some(Forgetting {
                peer,
                upto: contact.then_some(bound),
            })
        })
        .collect();
    let (core, initial) = Core::restore(seed, device, &snapshot, now_ms(), OsRng)?;
    Ok(Restored {
        store,
        files,
        forgetting,
        core,
        initial,
    })
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
        let Restored {
            store,
            files,
            forgetting,
            core,
            initial,
        } = blocking(move || restore(&restoring, device, &dir, &db)).await?;
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
        // A forget that cannot finish now keeps its marker for the next
        // start rather than keep the client from starting.
        for Forgetting { peer, upto } in forgetting {
            let finished = match upto {
                Some(upto) => client.delete_conversation(peer, upto).await,
                None => client.forget_peer(peer).await,
            };
            if let Err(e) = finished {
                tracing::warn!("finishing forgetting a contact failed: {e}");
            }
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
        let file_id = FileId::random(&mut OsRng);
        let file = Pick {
            file_id,
            path: path.to_owned(),
            name,
        };
        let (track, msg_id) = self.prepare_offer(peer, file, mime, kind).await?;
        self.send(track).await?;
        Ok((msg_id, file_id))
    }

    /// Sends a copy the app made only for sending (such as an Android
    /// `content://` pick) as `name`. The client takes the copy over and
    /// deletes it when the transfer ends, or at once when the offer fails.
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
        let file = Pick {
            file_id,
            path: path.clone(),
            name: name.to_owned(),
        };
        let prepared = async {
            if tokio::fs::rename(staged, &path).await.is_err() {
                // Another file system: copy, then drop the original.
                tokio::fs::copy(staged, &path).await?;
                tokio::fs::remove_file(staged).await?;
            }
            self.prepare_offer(peer, file, mime, MediaKind::File).await
        }
        .await;
        let (track, msg_id) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                discard_copy(&path).await;
                return Err(e);
            }
        };
        self.hand_over(track, &path).await?;
        Ok((msg_id, file_id))
    }

    /// Hands the driver `track`, which names the copy at `path`. Only a
    /// request the driver never got comes back (the channel returns it):
    /// then nothing will ever name the copy, and it is deleted. Once the
    /// driver has it, the copy is the transfer's, whatever happens next.
    async fn hand_over(&self, track: Request, path: &Path) -> Result<(), ClientError> {
        if self.tx.send(track).await.is_err() {
            discard_copy(path).await;
            return Err(ClientError::Closed);
        }
        Ok(())
    }

    /// The request that records `file` as the source of its transfer and
    /// then offers it, and the offer's message id.
    async fn prepare_offer(
        &self,
        peer: PeerId,
        Pick {
            file_id,
            path,
            name,
        }: Pick,
        mime: &str,
        kind: MediaKind,
    ) -> Result<(Request, MsgId), ClientError> {
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
        let track = Request::Track {
            file_id,
            entry: FileEntry {
                path,
                source: Some(source),
            },
            then: Some(cmd),
        };
        Ok((track, msg_id))
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
        let written = async {
            tokio::fs::write(&path, data).await?;
            SourceStamp::of(&tokio::fs::metadata(&path).await?)
        }
        .await;
        let source = match written {
            Ok(source) => source,
            Err(e) => {
                discard_copy(&path).await;
                return Err(e.into());
            }
        };
        let track = Request::Track {
            file_id,
            entry: FileEntry {
                path: path.clone(),
                source: Some(source),
            },
            then: Some(cmd),
        };
        self.hand_over(track, &path).await?;
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
    /// A contact row that cannot be read still lists its contact, unnamed;
    /// a message row that cannot be read is left out.
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
                    // A row that cannot be read is left out, not the list.
                    let readable: Vec<_> = rows
                        .iter()
                        .filter_map(|(key, value, status)| {
                            vault
                                .open_message_with_status(key, value, status.as_deref())
                                .ok()
                        })
                        .collect();
                    ui::summary(&peer, contact, &readable)
                })
                .collect();
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
    /// saved. The saved files themselves stay where they are. It goes a page
    /// at a time; a marker kept until the last page is gone lets the next
    /// start finish what a crash or a failure cut short.
    pub async fn forget_peer(&self, peer: PeerId) -> Result<(), ClientError> {
        // The marker names the newest message too: should the contact come
        // back before this finishes, what came after it is spared.
        let (first, last) = peer_messages(&peer);
        let newest = self
            .store
            .range_desc(Table::Messages.into(), first, End::Through(last.clone()), 1)
            .await?
            .into_iter()
            .next()
            .map(|(key, _)| key)
            .unwrap_or_default();
        let mark = store::Op::Put(FORGET_TABLE, peer.to_vec(), newest);
        self.store.apply(vec![mark]).await?;
        self.command(Command::RemovePeer { peer }).await?;
        self.delete_conversation(peer, last).await
    }

    /// Deletes the messages with `peer` up to the key `upto`, a page at a
    /// time, then the marker of its forgetting.
    async fn delete_conversation(&self, peer: PeerId, upto: Vec<u8>) -> Result<(), ClientError> {
        let (store, vault) = (self.store.clone(), Arc::clone(&self.vault));
        let config = self.config.clone();
        blocking(move || {
            let (first, _) = peer_messages(&peer);
            loop {
                // Nothing lies before `first`: an empty bound takes nothing.
                let page = store.blocking_range(
                    Table::Messages.into(),
                    first.clone(),
                    End::Through(upto.clone()),
                    FORGET_PAGE,
                )?;
                if page.is_empty() {
                    let unmark = store::Op::Delete(FORGET_TABLE, peer.to_vec());
                    return store.blocking_apply(vec![unmark]);
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
            .find(|(key, _)| message_id(key) == Some(msg_id))
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

/// Where the copies of files being sent are kept, in the client's folder.
fn outgoing_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("outgoing")
}

/// Deletes a copy in `outgoing/` whose offer failed: no transfer names it,
/// so nothing else would. One already gone is fine.
async fn discard_copy(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

/// Deletes the copies in `outgoing/` that no file row names (`named`): a
/// crash after a copy was taken over for sending, before it was recorded,
/// leaves one behind. Only regular files there are touched; the folder is
/// the client's own. One that cannot be read or deleted is logged and left
/// for the next start; the others still go.
fn sweep_outgoing(data_dir: &Path, named: &HashSet<FileId>) {
    let entries = match std::fs::read_dir(outgoing_dir(data_dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => return tracing::warn!("reading the outgoing folder failed: {e}"),
    };
    for entry in entries {
        let orphan = entry.and_then(|entry| {
            let name = entry.file_name();
            let file_id = name
                .to_str()
                .and_then(|n| n.strip_suffix(".bin"))
                .and_then(FileId::from_hex);
            let named = file_id.is_some_and(|id| named.contains(&id));
            Ok((!named && entry.file_type()?.is_file()).then(|| entry.path()))
        });
        let removed = match orphan {
            Ok(Some(path)) => std::fs::remove_file(path),
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = removed {
            tracing::warn!("deleting a copy no transfer names failed: {e}");
        }
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

fn load_files(
    rows: cypher_core::Rows,
    vault: &Vault,
) -> Result<HashMap<FileId, FileEntry>, ClientError> {
    let mut files = HashMap::new();
    for (k, v) in rows {
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

    /// Client failures reach the UI by the kind it words them by, with the
    /// error's own text as the message.
    #[test]
    fn client_errors_become_the_kinds_the_ui_words() {
        let cases = [
            (ClientError::Closed, ErrorKind::Stopped),
            (ClientError::InvalidInput, ErrorKind::Invalid),
            (ClientError::Core(CoreError::Invalid), ErrorKind::Invalid),
            (ClientError::NotFound, ErrorKind::NotFound),
            (ClientError::NotReady, ErrorKind::NotReady),
            (ClientError::Expired, ErrorKind::Expired),
            (ClientError::TorUnavailable, ErrorKind::Unsupported),
            (
                ClientError::Core(CoreError::NewerStorage),
                ErrorKind::Failed(FailReason::UpdateRequired),
            ),
            (
                ClientError::Identity(IdentityKind::Exists),
                ErrorKind::Identity(IdentityKind::Exists),
            ),
            (ClientError::Core(CoreError::Storage), ErrorKind::Internal),
        ];
        for (e, kind) in cases {
            let message = e.to_string();
            assert_eq!(CmdError::from(e), CmdError::new(kind, message));
        }
    }

    /// An identity file fails with the kind the web client reports for the
    /// same failure: both map it through the core.
    #[test]
    fn identity_file_failures_are_the_kinds_both_clients_report() {
        use cypher_crypto::identity_file::IdentityFileError;
        let all = [
            (
                IdentityFileError::WeakPassphrase,
                IdentityKind::WeakPassphrase,
            ),
            (
                IdentityFileError::InvalidNickname,
                IdentityKind::NicknameTooLong,
            ),
            (
                IdentityFileError::WrongPassphrase,
                IdentityKind::WrongPassphrase,
            ),
            (IdentityFileError::Corrupt, IdentityKind::Corrupt),
        ];
        for (e, kind) in all {
            let native = CmdError::from(ClientError::from(e));
            assert_eq!(native.kind, ErrorKind::Identity(kind));
        }
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
        put_status(&client, MsgId([9; 16]), MessageStatus::Read).await;
        let voice = MediaKind::Voice {
            duration_ms: 1,
            waveform: vec![1],
        };
        put(&client, &file(peer, 5, 7, voice)).await;
        put_note(&client, FileId([7; 16])).await;

        client.forget_peer(peer).await.unwrap();
        assert!(keys(&client, Table::Messages.into()).await.is_empty());
        let statuses = keys(&client, Table::MessageStatus.into()).await;
        assert!(statuses.is_empty(), "an unreadable row's status goes too");
        assert!(keys(&client, Table::Media.into()).await.is_empty());
        assert!(!client.config.media_path(&FileId([7; 16])).exists());
        client.shutdown().await;
    }

    /// The status row the core writes when a message is read reaches the
    /// store under the key history looks it up by.
    #[tokio::test]
    async fn a_status_the_core_writes_is_the_one_history_shows() {
        let dir = tempfile::tempdir().unwrap();
        let client = started(dir.path()).await;
        let peer = PeerId([4; 32]);
        put(&client, &text(peer, 1, 1)).await;
        let read = Command::MarkRead {
            peer,
            ids: vec![MsgId([1; 16])],
        };
        client.command(read).await.unwrap();
        let mut status = MessageStatus::Delivered;
        for _ in 0..100 {
            status = client.history(peer, None, 1).await.unwrap()[0].status;
            if status == MessageStatus::Read {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(status, MessageStatus::Read);
        let statuses = keys(&client, Table::MessageStatus.into()).await;
        assert_eq!(statuses, [vec![1; 16]]);
        client.shutdown().await;
    }

    /// A message row that cannot be read is left out of the chat list
    /// instead of failing it.
    #[tokio::test]
    async fn the_chat_list_passes_over_a_row_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        let client = started(dir.path()).await;
        let peer = PeerId([4; 32]);
        let contact = store::Op::Put(Table::Peers.into(), peer.to_vec(), vec![0; 40]);
        let unreadable = message_key(&peer, 9, &MsgId([9; 16]));
        let broken = store::Op::Put(Table::Messages.into(), unreadable, b"x".to_vec());
        client.store.apply(vec![contact, broken]).await.unwrap();
        put(&client, &text(peer, 1, 1)).await;

        let list = client.conversations().await.unwrap();
        let shown: Vec<_> = list
            .iter()
            .map(|c| (c.peer_id.clone(), c.last_message_at, c.unread))
            .collect();
        assert_eq!(shown, [(peer.to_hex(), 1, 1)]);
        client.shutdown().await;
    }

    /// Forgetting a contact that a crash cut short is finished at the next
    /// start: nothing of the conversation stays behind for good, and the
    /// contact's next message starts a fresh one.
    #[tokio::test]
    async fn a_forget_cut_short_is_finished_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let seed = IdentitySeed::generate();
        let start = || Client::start(&seed, DeviceId::FIRST, config(dir.path()));
        let (peer, other) = (PeerId([4; 32]), PeerId([5; 32]));
        let (client, _events) = start().await.unwrap();
        for msg in [text(peer, 1, 1), text(peer, 2, 2), text(other, 1, 3)] {
            put(&client, &msg).await;
            put_status(&client, msg.msg_id, MessageStatus::Read).await;
        }
        // Where a crash right after the contact was removed leaves it.
        let marker = store::Op::Put(FORGET_TABLE, peer.to_vec(), Vec::new());
        client.store.apply(vec![marker]).await.unwrap();
        client.shutdown().await;
        drop(client);

        let (client, _events) = start().await.unwrap();
        assert!(client.history(peer, None, 10).await.unwrap().is_empty());
        assert_eq!(
            keys(&client, Table::MessageStatus.into()).await,
            [vec![3; 16]]
        );
        assert!(
            keys(&client, FORGET_TABLE).await.is_empty(),
            "done for good"
        );
        assert_eq!(client.history(other, None, 10).await.unwrap().len(), 1);

        // A forget that runs to the end leaves no marker either.
        client.forget_peer(other).await.unwrap();
        assert!(keys(&client, FORGET_TABLE).await.is_empty());
        assert!(keys(&client, Table::Messages.into()).await.is_empty());
        client.shutdown().await;
    }

    /// Marks `peer` as being forgotten up to the message at `upto_ms`, the
    /// way a forget that failed after removing the contact leaves it.
    async fn mark_forgetting(client: &Client, peer: PeerId, upto_ms: u64, id: u8) {
        let bound = message_key(&peer, upto_ms, &MsgId([id; 16]));
        let marker = store::Op::Put(FORGET_TABLE, peer.to_vec(), bound);
        client.store.apply(vec![marker]).await.unwrap();
    }

    /// A contact taken back (or writing again as a request) after a forget
    /// that did not finish keeps what came after it: the next start deletes
    /// only what the forget was about.
    #[tokio::test]
    async fn a_forget_left_unfinished_spares_what_came_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let seed = IdentitySeed::generate();
        let start = || Client::start(&seed, DeviceId::FIRST, config(dir.path()));
        let peer = PeerId([4; 32]);
        let (client, _events) = start().await.unwrap();
        put(&client, &text(peer, 1, 1)).await;
        mark_forgetting(&client, peer, 1, 1).await;
        // The contact is back, and wrote again.
        let row = store::Op::Put(Table::Peers.into(), peer.to_vec(), vec![0; 40]);
        client.store.apply(vec![row]).await.unwrap();
        put(&client, &text(peer, 5, 5)).await;
        client.shutdown().await;
        drop(client);

        let (client, _events) = start().await.unwrap();
        let left: Vec<_> = client
            .history(peer, None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.msg_id)
            .collect();
        assert_eq!(left, [MsgId([5; 16])], "only what came after is kept");
        assert_eq!(keys(&client, Table::Peers.into()).await, [peer.to_vec()]);
        assert!(keys(&client, FORGET_TABLE).await.is_empty());
        client.shutdown().await;
    }

    /// A forget that cannot be finished at start does not stop the client
    /// from starting; its marker stays for the next try.
    #[tokio::test]
    async fn a_forget_that_cannot_finish_does_not_stop_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let seed = IdentitySeed::generate();
        let start = || Client::start(&seed, DeviceId::FIRST, config(dir.path()));
        let peer = PeerId([4; 32]);
        let (client, _events) = start().await.unwrap();
        let voice = MediaKind::Voice {
            duration_ms: 1,
            waveform: vec![1],
        };
        put(&client, &file(peer, 1, 7, voice)).await;
        mark_forgetting(&client, peer, 1, 7).await;
        // A sealed copy that cannot be deleted.
        let stuck = client.config.media_path(&FileId([7; 16]));
        std::fs::create_dir_all(stuck.join("inside")).unwrap();
        client.shutdown().await;
        drop(client);

        let (client, _events) = start().await.unwrap();
        assert_eq!(keys(&client, FORGET_TABLE).await, [peer.to_vec()]);
        client.shutdown().await;
    }

    /// The files in the app's `outgoing/` folder, by name.
    fn outgoing_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir.join("outgoing")).map_or_else(
            |_| Vec::new(),
            |entries| {
                let mut names: Vec<_> = entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            },
        )
    }

    /// A copy taken over for sending is deleted when its offer fails, so
    /// nothing is left in `outgoing/` that no transfer will ever remove.
    #[tokio::test]
    async fn a_copy_for_sending_is_deleted_when_its_offer_fails() {
        let dir = tempfile::tempdir().unwrap();
        let client = started(dir.path()).await;
        // A stopped client refuses every offer.
        client.shutdown().await;
        let peer = PeerId([4; 32]);
        let staged = dir.path().join("pick.tmp");
        std::fs::write(&staged, b"picked").unwrap();
        let refused = client
            .send_staged_file(peer, &staged, "pick.txt", "text/plain")
            .await;
        assert!(matches!(refused, Err(ClientError::Closed)), "{refused:?}");
        assert!(!staged.exists(), "the client took the copy over");
        assert!(outgoing_files(dir.path()).is_empty());

        let voice = MediaKind::Voice {
            duration_ms: 1,
            waveform: vec![1],
        };
        let note = vec![7; MAX_INLINE_LEN + 1];
        let refused = client
            .send_media(peer, note, "voice.ogg", "audio/ogg", voice)
            .await;
        assert!(matches!(refused, Err(ClientError::Closed)), "{refused:?}");
        assert!(outgoing_files(dir.path()).is_empty());
    }

    /// Once the driver has an offer, its copy is the transfer's: it stays,
    /// recorded, whatever the transfer does next (here: no server to reach).
    #[tokio::test]
    async fn a_copy_whose_offer_reached_the_driver_stays() {
        let dir = tempfile::tempdir().unwrap();
        let client = started(dir.path()).await;
        let staged = dir.path().join("pick.tmp");
        std::fs::write(&staged, b"picked").unwrap();
        let peer = PeerId([4; 32]);
        let (_, file_id) = client
            .send_staged_file(peer, &staged, "pick.txt", "text/plain")
            .await
            .unwrap();
        client.shutdown().await;
        let copy = format!("{}.bin", file_id.to_hex());
        assert_eq!(outgoing_files(dir.path()), [copy]);
        assert_eq!(keys(&client, FILES_TABLE).await, [file_id.to_vec()]);
    }

    /// A start deletes the copies in `outgoing/` no transfer names (a crash
    /// between taking a copy over and recording it leaves one), and nothing
    /// else.
    #[tokio::test]
    async fn a_start_deletes_copies_no_transfer_names() {
        let dir = tempfile::tempdir().unwrap();
        let seed = IdentitySeed::generate();
        let start = || Client::start(&seed, DeviceId::FIRST, config(dir.path()));
        let (client, _events) = start().await.unwrap();
        let (kept, orphan) = (FileId([1; 16]), FileId([2; 16]));
        let kept_path = client.config.outgoing_path(&kept);
        let entry = FileEntry {
            path: kept_path.clone(),
            source: None,
        };
        let sealed = client
            .vault
            .seal(Table::Meta, &file_key(&kept), &entry, &mut OsRng);
        let op = store::Op::Put(FILES_TABLE, kept.to_vec(), sealed);
        client.store.apply(vec![op]).await.unwrap();
        let orphan_path = client.config.outgoing_path(&orphan);
        std::fs::create_dir_all(orphan_path.parent().unwrap()).unwrap();
        for path in [&kept_path, &orphan_path] {
            std::fs::write(path, b"plain").unwrap();
        }
        // Not the client's to delete: elsewhere in its folder, or a folder.
        let elsewhere = dir.path().join("keep.bin");
        std::fs::write(&elsewhere, b"user").unwrap();
        std::fs::create_dir_all(dir.path().join("outgoing").join("folder")).unwrap();
        client.shutdown().await;
        drop(client);

        let (client, _events) = start().await.unwrap();
        let kept_name = format!("{}.bin", kept.to_hex());
        assert_eq!(outgoing_files(dir.path()), [kept_name, "folder".to_owned()]);
        assert!(elsewhere.exists());
        client.shutdown().await;
    }
}
