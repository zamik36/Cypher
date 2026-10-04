//! Native Cypher client: a tokio driver around the sans-IO `cypher-core`,
//! with SQLite persistence, positional file IO and automatic reconnects.

mod driver;
mod files;
pub mod identity;
mod media;
mod net;
mod store;
#[cfg(feature = "tor")]
mod tor;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cypher_core::envelope::MAX_INLINE_LEN;
use cypher_core::{
    Command, Core, CoreError, Event, MediaKind, MessageStatus, Snapshot, StoredMessage, Table,
    Vault, message_key,
};
use cypher_crypto::IdentitySeed;
use cypher_types::{FileId, MsgId, PeerId};
use rand::rngs::OsRng;
use tokio::sync::mpsc;

use driver::{Driver, FileEntry, Parts, file_key, now_ms};
use files::SourceStamp;
use store::{FILES_TABLE, Store};

pub use cypher_core::{Content, FailReason};
pub use identity::{IdentityStore, Unlocked};
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
    /// Reach the onion relay through Tor (requires the `tor` feature).
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
}

pub(crate) enum Request {
    Command(Command),
    /// Records where a transfer lives, then optionally runs a command.
    Track {
        file_id: FileId,
        entry: FileEntry,
        then: Option<Command>,
    },
    /// Connects again after the session was taken over by another device.
    Reconnect,
    Shutdown,
}

/// Someone the user has a session with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub peer: PeerId,
    /// The name the user gave them on this device.
    pub alias: Option<String>,
}

#[derive(Clone)]
pub struct Client {
    tx: mpsc::Sender<Request>,
    store: Store,
    vault: Arc<Vault>,
    peer_id: PeerId,
    config: Config,
}

impl Client {
    /// Restores persisted state for `seed` and starts connecting.
    pub async fn start(
        seed: &IdentitySeed,
        config: Config,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Event>), ClientError> {
        let peer_id = seed.derive_identity().peer_id();
        std::fs::create_dir_all(&config.data_dir)?;
        let hex = peer_id.to_hex();
        let db = config
            .data_dir
            .join(format!("state-{}.db", hex.get(..16).unwrap_or(&hex)));
        let store = Store::open(&db)?;

        let snapshot = Snapshot {
            meta: store.scan(Table::Meta.name()).await?,
            peers: store.scan(Table::Peers.name()).await?,
            outbox: store.scan(Table::Outbox.name()).await?,
            transfers: store.scan(Table::Transfers.name()).await?,
        };
        let vault = Vault::new(seed.derive_storage_key());
        let files = load_files(&store, &vault).await?;
        let (core, initial) = Core::restore(seed, &snapshot, now_ms(), OsRng)?;

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (req_tx, req_rx) = mpsc::channel(256);
        Driver::spawn(
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
            store,
            vault: Arc::new(vault),
            peer_id,
            config,
        };
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

    pub async fn send_text(&self, peer: PeerId, text: String) -> Result<MsgId, ClientError> {
        let msg_id = MsgId::random(&mut OsRng);
        self.command(Command::SendText {
            peer,
            msg_id,
            text,
            reply_to: None,
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
        let source = SourceStamp::of(&tokio::fs::metadata(path).await?)?;
        let size = source.len();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or(ClientError::InvalidInput)?;
        let inline = if kind.is_media() && size <= MAX_INLINE_LEN as u64 {
            Some(tokio::fs::read(path).await?)
        } else {
            None
        };
        let (msg_id, file_id) = (MsgId::random(&mut OsRng), FileId::random(&mut OsRng));
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
                path: path.to_owned(),
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
            .get(Table::Media.name(), file_id.to_vec())
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

    /// Conversation history with `peer`, newest first, sent before `before_ms`.
    pub async fn history(
        &self,
        peer: PeerId,
        before_ms: Option<u64>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, ClientError> {
        let from = message_key(&peer, 0, &MsgId([0; 16]));
        let to = message_key(&peer, before_ms.unwrap_or(u64::MAX), &MsgId([0; 16]));
        let rows = self
            .store
            .range_desc(Table::Messages.name(), from, to, limit)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (key, value) in rows {
            let mut msg = self.vault.open_message(&key, &value)?;
            if let Some(status) = self
                .store
                .get(Table::MessageStatus.name(), msg.msg_id.to_vec())
                .await?
                .and_then(|raw| {
                    self.vault
                        .open::<MessageStatus>(Table::MessageStatus, msg.msg_id.as_bytes(), &raw)
                        .ok()
                })
            {
                msg.status = status;
            }
            out.push(msg);
        }
        Ok(out)
    }

    /// Peers with an established session.
    /// Everyone with a session, with the name the user gave them. A row that
    /// cannot be read still lists its contact, unnamed.
    pub async fn contacts(&self) -> Result<Vec<Contact>, ClientError> {
        Ok(self
            .store
            .scan(Table::Peers.name())
            .await?
            .into_iter()
            .filter_map(|(key, sealed)| {
                let peer = PeerId::from_bytes(&key)?;
                let alias = self.vault.open_contact_alias(&key, &sealed).ok().flatten();
                Some(Contact { peer, alias })
            })
            .collect())
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
    /// their statuses and stored voice and video notes. Files the user saved
    /// stay where they are.
    pub async fn forget_peer(&self, peer: PeerId) -> Result<(), ClientError> {
        self.command(Command::RemovePeer { peer }).await?;
        let from = message_key(&peer, 0, &MsgId([0; 16]));
        let to = message_key(&peer, u64::MAX, &MsgId([0xFF; 16]));
        let rows = self
            .store
            .range_desc(Table::Messages.name(), from, to, usize::MAX)
            .await?;
        let mut ops = Vec::with_capacity(rows.len() * 2);
        let mut media = Vec::new();
        for (key, sealed) in rows {
            if let Ok(msg) = self.vault.open_message(&key, &sealed) {
                ops.push(store::Op::Delete(
                    Table::MessageStatus.name(),
                    msg.msg_id.to_vec(),
                ));
                if let Content::File { file_id, kind, .. } = msg.content
                    && kind.is_media()
                {
                    ops.push(store::Op::Delete(Table::Media.name(), file_id.to_vec()));
                    media.push(file_id);
                }
            }
            ops.push(store::Op::Delete(Table::Messages.name(), key));
        }
        self.store.apply(ops).await?;
        for file_id in media {
            match tokio::fs::remove_file(self.config.media_path(&file_id)).await {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    /// Deletes every stored message and media note; sessions and contacts
    /// are kept.
    pub async fn clear_history(&self) -> Result<(), ClientError> {
        self.store
            .apply(vec![
                store::Op::Clear(Table::Messages.name()),
                store::Op::Clear(Table::MessageStatus.name()),
                store::Op::Clear(Table::Media.name()),
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

    pub async fn shutdown(&self) {
        let _ = self.tx.send(Request::Shutdown).await;
    }

    async fn send(&self, req: Request) -> Result<(), ClientError> {
        self.tx.send(req).await.map_err(|_| ClientError::Closed)
    }
}

async fn load_files(
    store: &Store,
    vault: &Vault,
) -> Result<HashMap<FileId, FileEntry>, ClientError> {
    let mut files = HashMap::new();
    for (k, v) in store.scan(FILES_TABLE).await? {
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
