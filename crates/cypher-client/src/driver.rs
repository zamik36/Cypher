use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cypher_core::{Core, Effect, Event, FailReason, Input, StoreOp};
use cypher_types::FileId;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

use crate::files::{ChunkRead, FileIo, IoDone, IoJob, SourceStamp};
use crate::net::{self, Link, NetEvent};
use crate::store::{FILES_TABLE, Op, SAVED_TABLE, Store};
use crate::{Config, Request};

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Where a transfer lives on disk; persisted so transfers resume after restart.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FileEntry {
    pub path: PathBuf,
    /// The file as offered, for files we send; `None` for files we receive.
    pub source: Option<SourceStamp>,
}

impl cypher_core::Record for FileEntry {
    const VERSION: u8 = 2;

    /// Version 1 had no stamp. Receiving still resumes; sending fails, as
    /// the file can no longer be checked against what was offered.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, cypher_core::CoreError> {
        match version {
            1 => Ok(Self {
                path: postcard::from_bytes(body).map_err(|_| cypher_core::CoreError::Storage)?,
                source: None,
            }),
            _ => Err(cypher_core::CoreError::Storage),
        }
    }
}

/// Where a finished file ended up: a path, or a URI once the platform moved
/// it to shared storage (Android).
#[derive(Serialize, Deserialize)]
pub(crate) struct SavedFile {
    pub location: String,
}

impl cypher_core::Record for SavedFile {
    const VERSION: u8 = 1;
}

pub(crate) struct Driver {
    core: Core<OsRng>,
    store: Store,
    io: FileIo,
    config: Config,
    events: mpsc::UnboundedSender<Event>,
    files: HashMap<FileId, FileEntry>,
    vault: cypher_core::Vault,
    gateway: Option<mpsc::Sender<Bytes>>,
    relay: Option<mpsc::Sender<Bytes>>,
    relay_addr: Option<String>,
    net_tx: mpsc::UnboundedSender<NetEvent>,
    gateway_retry: Retry,
    relay_retry: Retry,
    reconnect: bool,
    ops: Vec<Op>,
    /// Set when state could not be persisted; the driver then stops.
    failed: bool,
    #[cfg(feature = "tor")]
    tor: Option<Arc<crate::tor::Tor>>,
}

struct Retry {
    pending: bool,
    at: Instant,
    backoff: Duration,
}

impl Retry {
    fn new() -> Self {
        Self {
            pending: false,
            at: Instant::now(),
            backoff: MIN_BACKOFF,
        }
    }

    /// Waits a random time in the upper half of the backoff, so clients
    /// dropped together (e.g. by a server restart) do not reconnect in step.
    fn schedule(&mut self) {
        let wait = rand::Rng::gen_range(&mut OsRng, self.backoff / 2..=self.backoff);
        self.at = Instant::now() + wait;
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    fn due(&self) -> bool {
        !self.pending && Instant::now() >= self.at
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// State the driver takes ownership of at startup.
pub(crate) struct Parts {
    pub core: Core<OsRng>,
    pub store: Store,
    pub config: Config,
    pub files: HashMap<FileId, FileEntry>,
    pub vault: cypher_core::Vault,
}

impl Driver {
    pub(crate) fn spawn(
        parts: Parts,
        events: mpsc::UnboundedSender<Event>,
        requests: mpsc::Receiver<Request>,
        initial: Vec<Effect>,
    ) -> std::io::Result<watch::Receiver<()>> {
        let (net_tx, net_rx) = mpsc::unbounded_channel();
        let (io_tx, io_rx) = mpsc::unbounded_channel();
        let driver = Self {
            core: parts.core,
            store: parts.store,
            io: FileIo::spawn(io_tx)?,
            config: parts.config,
            events,
            files: parts.files,
            vault: parts.vault,
            gateway: None,
            relay: None,
            relay_addr: None,
            net_tx,
            gateway_retry: Retry::new(),
            relay_retry: Retry::new(),
            reconnect: true,
            ops: Vec::new(),
            failed: false,
            #[cfg(feature = "tor")]
            tor: None,
        };
        // Dropped when the driver is done: its last state is on disk.
        let (running, stopped) = watch::channel(());
        tokio::spawn(async move {
            driver.run(requests, net_rx, io_rx, initial).await;
            drop(running);
        });
        Ok(stopped)
    }

    async fn run(
        mut self,
        mut requests: mpsc::Receiver<Request>,
        mut net_rx: mpsc::UnboundedReceiver<NetEvent>,
        mut io_rx: mpsc::UnboundedReceiver<IoDone>,
        initial: Vec<Effect>,
    ) {
        self.apply(initial).await;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                req = requests.recv() => match req {
                    Some(Request::Shutdown) | None => break,
                    Some(req) => self.on_request(req).await,
                },
                Some(ev) = net_rx.recv() => self.on_net(ev).await,
                Some(done) = io_rx.recv() => self.on_io(done).await,
                _ = tick.tick() => self.on_tick().await,
            }
            if self.failed {
                break;
            }
        }
        self.flush().await;
    }

    async fn on_request(&mut self, req: Request) {
        match req {
            Request::Command(cmd) => self.feed(Input::Command(cmd)).await,
            Request::Track {
                file_id,
                entry,
                then,
            } => {
                self.remember_file(file_id, entry);
                if let Some(cmd) = then {
                    self.feed(Input::Command(cmd)).await;
                }
            }
            Request::Saved {
                file_id,
                location,
                done,
            } => {
                self.remember_saved(file_id, location);
                if self.flush().await {
                    let _ = done.send(());
                }
            }
            Request::Reconnect => {
                self.reconnect = true;
                self.gateway_retry = Retry::new();
            }
            Request::Shutdown => {}
        }
    }

    async fn on_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Up(Link::Gateway, tx) => {
                self.gateway_retry.pending = false;
                self.gateway = Some(tx);
                self.feed(Input::Connected).await;
            }
            NetEvent::Frame(Link::Gateway, frame) => self.feed(Input::Frame(frame)).await,
            NetEvent::Down(Link::Gateway) => {
                self.gateway = None;
                self.gateway_retry.pending = false;
                self.gateway_retry.schedule();
                self.feed(Input::Disconnected).await;
            }
            NetEvent::Up(Link::Relay, tx) => {
                self.relay_retry.pending = false;
                self.relay_retry.backoff = MIN_BACKOFF;
                self.relay = Some(tx);
                self.feed(Input::AnonymousChannel { up: true }).await;
            }
            NetEvent::Frame(Link::Relay, frame) => self.feed(Input::AnonymousFrame(frame)).await,
            NetEvent::Down(Link::Relay) => {
                self.relay = None;
                self.relay_retry.pending = false;
                self.relay_retry.schedule();
                self.feed(Input::AnonymousChannel { up: false }).await;
            }
        }
    }

    async fn on_io(&mut self, done: IoDone) {
        let input = match done {
            IoDone::Notify(event) => {
                let _ = self.events.send(event);
                return;
            }
            IoDone::ChunkRead {
                file_id,
                index,
                buf,
            } => Input::ChunkRead {
                file_id,
                index,
                buf,
            },
            IoDone::ReadFailed { file_id } | IoDone::WriteFailed { file_id } => {
                Input::Command(cypher_core::Command::CancelTransfer { file_id })
            }
        };
        self.feed(input).await;
    }

    async fn on_tick(&mut self) {
        if self.reconnect && self.gateway.is_none() && self.gateway_retry.due() {
            self.gateway_retry.pending = true;
            net::spawn(
                Link::Gateway,
                self.config.gateway_addr.clone(),
                Arc::clone(&self.config.tls),
                self.net_tx.clone(),
            );
        }
        if let Some(addr) = self.relay_addr.clone()
            && self.relay.is_none()
            && self.relay_retry.due()
        {
            self.relay_retry.pending = true;
            self.spawn_relay(addr);
        }
        self.feed(Input::Tick).await;
    }

    #[cfg_attr(
        not(feature = "tor"),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "the Tor build caches its client in self"
        )
    )]
    fn spawn_relay(&mut self, addr: String) {
        #[cfg(feature = "tor")]
        if let Some(config) = &self.config.tor {
            let tor = self.tor.get_or_insert_with(|| {
                crate::tor::Tor::new(config.clone(), self.config.data_dir.join("tor"))
            });
            tor.spawn_relay(addr, Arc::clone(&self.config.tls), self.net_tx.clone());
            return;
        }
        #[cfg(not(feature = "tor"))]
        if self.config.tor.is_some() {
            tracing::warn!("built without Tor support; connecting to the relay directly");
        }
        net::spawn(
            Link::Relay,
            addr,
            Arc::clone(&self.config.tls),
            self.net_tx.clone(),
        );
    }

    async fn feed(&mut self, input: Input) {
        if self.failed {
            return;
        }
        let effects = self.core.handle(input, now_ms());
        self.apply(effects).await;
    }

    /// Executes effects in order. Pending persistence is committed before any
    /// frame leaves, so a crash can never replay ratchet keys or lose data the
    /// server already considers delivered. If it cannot be committed, nothing
    /// after it runs: the core has moved past what is on disk.
    async fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Persist(op) => self.stage(op),
                Effect::Transmit(frame) => {
                    if !self.flush().await {
                        return;
                    }
                    if let Some(gw) = &self.gateway {
                        let _ = gw.send(frame).await;
                    }
                }
                Effect::Anonymous(frame) => {
                    if !self.flush().await {
                        return;
                    }
                    if let Some(relay) = &self.relay {
                        let _ = relay.send(frame).await;
                    }
                }
                Effect::Emit(event) => self.emit(event),
                Effect::ReadChunk {
                    file_id,
                    index,
                    offset,
                    len,
                    headroom,
                } => self.read_chunk(file_id, index, offset, len, headroom).await,
                Effect::Disconnect { reconnect } => {
                    self.reconnect = reconnect;
                    self.gateway = None;
                }
                effect @ (Effect::OpenSink { .. }
                | Effect::WriteChunk { .. }
                | Effect::CloseSink { .. }) => {
                    if let Some(job) = self.sink_job(effect) {
                        self.io.submit(job);
                    }
                }
            }
        }
        self.flush().await;
    }

    /// File IO for a sink effect; `None` when the transfer's target is unknown.
    fn sink_job(&self, effect: Effect) -> Option<IoJob> {
        Some(match effect {
            Effect::OpenSink {
                file_id,
                len,
                sealed,
            } => {
                // A sealed copy always lives at `media_path`; `files` keeps
                // pointing at the transfer's plaintext source or target.
                let path = if sealed {
                    self.config.media_path(&file_id)
                } else {
                    self.files.get(&file_id)?.path.clone()
                };
                IoJob::Open { file_id, path, len }
            }
            Effect::WriteChunk {
                file_id,
                offset,
                data,
            } => IoJob::Write {
                file_id,
                offset,
                data,
            },
            Effect::CloseSink { file_id, complete } => IoJob::Close {
                file_id,
                keep: complete,
            },
            _ => return None,
        })
    }

    fn emit(&mut self, event: Event) {
        if let Event::Bootstrap { relay_addr, .. } = &event
            && !relay_addr.is_empty()
        {
            self.relay_addr = Some(relay_addr.clone());
        }
        if let Event::Connected = event {
            self.gateway_retry.backoff = MIN_BACKOFF;
        }
        if let Event::TransferComplete { file_id } = &event {
            let file_id = *file_id;
            let received = self
                .files
                .get(&file_id)
                .is_some_and(|entry| entry.source.is_none());
            self.note_saved(file_id);
            self.release_file(file_id);
            if received {
                // Told only once the file is closed on disk: the IO thread
                // runs jobs in order, and the sink's close is already queued.
                self.io.submit(IoJob::Notify(event));
                return;
            }
        } else if let Event::TransferFailed { file_id, .. } = &event {
            self.release_file(*file_id);
        }
        let _ = self.events.send(event);
    }

    fn stage(&mut self, op: StoreOp) {
        self.ops.push(op.into());
    }

    /// Commits staged operations. On failure the client stops for good:
    /// sending anything computed from unsaved state could reuse ratchet keys
    /// after a restart, or acknowledge inbox items that were never stored.
    async fn flush(&mut self) -> bool {
        if self.failed {
            return false;
        }
        let ops = std::mem::take(&mut self.ops);
        let Err(e) = self.store.apply(ops).await else {
            return true;
        };
        tracing::error!("persisting client state failed, stopping the client: {e}");
        self.failed = true;
        self.reconnect = false;
        self.relay_addr = None;
        self.gateway = None;
        self.relay = None;
        let _ = self.events.send(Event::Warning {
            reason: FailReason::StorageFailed,
        });
        let _ = self.events.send(Event::Disconnected);
        false
    }

    /// Reads a chunk of a file we send. One whose file is unknown, or cannot
    /// be checked against what was offered, fails its transfer instead.
    async fn read_chunk(
        &mut self,
        file_id: FileId,
        index: u32,
        offset: u64,
        len: u32,
        headroom: usize,
    ) {
        let Some(FileEntry {
            path,
            source: Some(source),
        }) = self.files.get(&file_id)
        else {
            let effects = self
                .core
                .handle(Input::ChunkUnavailable { file_id }, now_ms());
            return Box::pin(self.apply(effects)).await;
        };
        self.io.submit(IoJob::Read(ChunkRead {
            file_id,
            path: path.clone(),
            source: *source,
            index,
            offset,
            len,
            headroom,
        }));
    }

    fn remember_file(&mut self, file_id: FileId, entry: FileEntry) {
        let value = self.vault.seal(
            cypher_core::Table::Meta,
            &file_key(&file_id),
            &entry,
            &mut OsRng,
        );
        self.ops.push(Op::Put(FILES_TABLE, file_id.to_vec(), value));
        self.files.insert(file_id, entry);
    }

    /// Remembers where a finished file is, unless it is a temporary or
    /// sealed copy the app manages itself.
    fn note_saved(&mut self, file_id: FileId) {
        let Some(entry) = self.files.get(&file_id) else {
            return;
        };
        if entry.path == self.config.outgoing_path(&file_id)
            || entry.path == self.config.media_path(&file_id)
        {
            return;
        }
        if let Some(location) = entry.path.to_str() {
            let location = location.to_owned();
            self.remember_saved(file_id, location);
        }
    }

    fn remember_saved(&mut self, file_id: FileId, location: String) {
        let value = self.vault.seal(
            cypher_core::Table::Meta,
            &saved_key(&file_id),
            &SavedFile { location },
            &mut OsRng,
        );
        self.ops.push(Op::Put(SAVED_TABLE, file_id.to_vec(), value));
    }

    /// Drops the bookkeeping of a finished transfer and deletes the staged
    /// plaintext of an outgoing recording.
    fn release_file(&mut self, file_id: FileId) {
        let Some(entry) = self.files.remove(&file_id) else {
            return;
        };
        self.ops.push(Op::Delete(FILES_TABLE, file_id.to_vec()));
        if entry.path == self.config.outgoing_path(&file_id) {
            self.io.submit(IoJob::Remove {
                file_id,
                path: entry.path,
            });
        }
    }
}

/// AAD key for file entries, distinct from every core record key.
pub(crate) fn file_key(file_id: &FileId) -> Vec<u8> {
    let mut k = b"file:".to_vec();
    k.extend_from_slice(file_id.as_bytes());
    k
}

/// AAD key for saved-file entries.
pub(crate) fn saved_key(file_id: &FileId) -> Vec<u8> {
    let mut k = b"saved:".to_vec();
    k.extend_from_slice(file_id.as_bytes());
    k
}

#[cfg(test)]
mod tests {
    use cypher_core::{Snapshot, Table};
    use cypher_crypto::IdentitySeed;

    use super::*;

    fn driver(store: Store) -> (Driver, mpsc::UnboundedReceiver<Event>) {
        let dir = tempfile::tempdir().unwrap();
        let seed = IdentitySeed::generate();
        let (core, _) = Core::restore(
            &seed,
            cypher_types::DeviceId::FIRST,
            &Snapshot::default(),
            now_ms(),
            OsRng,
        )
        .unwrap();
        let (events, events_rx) = mpsc::unbounded_channel();
        let (net_tx, _) = mpsc::unbounded_channel();
        let (io_tx, _) = mpsc::unbounded_channel();
        let driver = Driver {
            core,
            store,
            io: FileIo::spawn(io_tx).unwrap(),
            config: Config {
                gateway_addr: "localhost:1".into(),
                tls: cypher_tls::make_client_config(),
                data_dir: dir.path().to_owned(),
                require_onion: false,
                tor: None,
            },
            events,
            files: HashMap::new(),
            vault: cypher_core::Vault::new(seed.derive_storage_key()),
            gateway: None,
            relay: None,
            relay_addr: Some("localhost:2".into()),
            net_tx,
            gateway_retry: Retry::new(),
            relay_retry: Retry::new(),
            reconnect: true,
            ops: Vec::new(),
            failed: false,
            #[cfg(feature = "tor")]
            tor: None,
        };
        (driver, events_rx)
    }

    fn persist() -> Effect {
        Effect::Persist(StoreOp::Put {
            table: Table::Meta,
            key: b"k".to_vec(),
            value: b"v".to_vec(),
        })
    }

    #[tokio::test]
    async fn frames_leave_only_after_their_state_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let (mut d, _events) = driver(Store::open(&dir.path().join("state.db")).unwrap());
        let (gateway, mut sent) = mpsc::channel(4);
        d.gateway = Some(gateway);
        d.apply(vec![
            persist(),
            Effect::Transmit(Bytes::from_static(b"frame")),
        ])
        .await;
        assert_eq!(sent.try_recv().unwrap(), Bytes::from_static(b"frame"));
        assert_eq!(
            d.store.get("meta", b"k".to_vec()).await.unwrap(),
            Some(b"v".to_vec())
        );
        assert!(!d.failed);
    }

    #[tokio::test]
    async fn a_failed_write_stops_the_client_before_anything_is_sent() {
        let (mut d, mut events) = driver(Store::detached());
        let (gateway, mut sent) = mpsc::channel(4);
        let (relay, mut anonymous) = mpsc::channel(4);
        d.gateway = Some(gateway);
        d.relay = Some(relay);
        d.apply(vec![
            persist(),
            Effect::Transmit(Bytes::from_static(b"frame")),
            Effect::Anonymous(Bytes::from_static(b"inbox ack")),
        ])
        .await;

        assert!(sent.try_recv().is_err(), "nothing reaches the gateway");
        assert!(anonymous.try_recv().is_err(), "nothing reaches the relay");
        assert!(d.failed && !d.reconnect && d.relay_addr.is_none());
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::Warning {
                reason: FailReason::StorageFailed
            }
        ));
        assert!(matches!(events.try_recv().unwrap(), Event::Disconnected));

        d.feed(Input::Tick).await;
        assert!(
            events.try_recv().is_err(),
            "a stopped client handles no input"
        );
    }

    /// A received file is announced only once its sink is closed, and is
    /// remembered where it was saved; a staged recording is neither delayed
    /// nor remembered.
    #[tokio::test]
    async fn a_received_file_is_announced_once_on_disk_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let (mut d, mut events) = driver(Store::open(&dir.path().join("state.db")).unwrap());
        let (io_tx, mut io_rx) = mpsc::unbounded_channel();
        d.io = FileIo::spawn(io_tx).unwrap();
        let (received, recorded) = (FileId([1; 16]), FileId([2; 16]));
        let path = dir.path().join("in.bin");
        d.files.insert(
            received,
            FileEntry {
                path: path.clone(),
                source: None,
            },
        );
        let staged = d.config.outgoing_path(&recorded);
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"note").unwrap();
        let stamp = SourceStamp::of(&std::fs::metadata(&staged).unwrap()).unwrap();
        d.files.insert(
            recorded,
            FileEntry {
                path: staged,
                source: Some(stamp),
            },
        );

        d.emit(Event::TransferComplete { file_id: received });
        assert!(
            events.try_recv().is_err(),
            "not before the IO thread is done"
        );
        assert!(d.flush().await);
        let done = io_rx.recv().await.unwrap();
        d.on_io(done).await;
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::TransferComplete { file_id } if file_id == received
        ));
        let sealed = d
            .store
            .get(SAVED_TABLE, received.to_vec())
            .await
            .unwrap()
            .unwrap();
        let saved: SavedFile = d
            .vault
            .open(Table::Meta, &saved_key(&received), &sealed)
            .unwrap();
        assert_eq!(saved.location, path.to_str().unwrap());

        d.emit(Event::TransferComplete { file_id: recorded });
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::TransferComplete { file_id } if file_id == recorded
        ));
        assert!(d.flush().await);
        assert_eq!(
            d.store.get(SAVED_TABLE, recorded.to_vec()).await.unwrap(),
            None
        );
    }

    /// After another device took the session over the client stays offline;
    /// asking to reconnect dials again at once.
    #[tokio::test]
    async fn a_session_taken_over_comes_back_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut d, _events) = driver(Store::open(&dir.path().join("state.db")).unwrap());
        d.apply(vec![Effect::Disconnect { reconnect: false }]).await;
        assert!(!d.reconnect);
        d.gateway_retry.schedule();
        assert!(!d.gateway_retry.due());

        d.on_request(Request::Reconnect).await;
        assert!(d.reconnect);
        assert!(d.gateway_retry.due());
    }

    /// Entries from before stamps still load: downloads resume, while an
    /// upload, having no stamp to check its file against, is not read.
    #[test]
    fn entries_from_before_stamps_load_without_one() {
        #[derive(Serialize, Deserialize)]
        struct V1 {
            path: PathBuf,
        }
        impl cypher_core::Record for V1 {
            const VERSION: u8 = 1;
        }

        let vault = cypher_core::Vault::new([7; 32]);
        let key = file_key(&FileId([5; 16]));
        let old = V1 {
            path: PathBuf::from("download.bin"),
        };
        let sealed = vault.seal(Table::Meta, &key, &old, &mut OsRng);
        let entry: FileEntry = vault.open(Table::Meta, &key, &sealed).unwrap();
        assert_eq!(entry.path, old.path);
        assert!(entry.source.is_none());
    }
}
