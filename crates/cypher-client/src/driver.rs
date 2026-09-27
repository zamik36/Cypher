use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cypher_core::{Core, Effect, Event, Input, StoreOp};
use cypher_types::FileId;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::files::{FileIo, IoDone, IoJob};
use crate::net::{self, Link, NetEvent};
use crate::store::{FILES_TABLE, Op, Store};
use crate::{Config, Request};

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Where a transfer lives on disk; persisted so transfers resume after restart.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FileEntry {
    pub path: PathBuf,
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
    #[cfg(feature = "tor")]
    tor: Option<std::sync::Arc<crate::tor::Tor>>,
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

    fn schedule(&mut self) {
        self.at = Instant::now() + self.backoff;
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
    pub fn spawn(
        parts: Parts,
        events: mpsc::UnboundedSender<Event>,
        requests: mpsc::Receiver<Request>,
        initial: Vec<Effect>,
    ) -> std::io::Result<()> {
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
            #[cfg(feature = "tor")]
            tor: None,
        };
        tokio::spawn(driver.run(requests, net_rx, io_rx, initial));
        Ok(())
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
        }
        self.flush().await;
    }

    async fn on_request(&mut self, req: Request) {
        match req {
            Request::Command(cmd) => self.feed(Input::Command(cmd)).await,
            Request::Track {
                file_id,
                path,
                then,
            } => {
                self.remember_file(file_id, path);
                if let Some(cmd) = then {
                    self.feed(Input::Command(cmd)).await;
                }
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
                self.config.tls.clone(),
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

    fn spawn_relay(&mut self, addr: String) {
        #[cfg(feature = "tor")]
        if let Some(config) = &self.config.tor {
            let tor = self.tor.get_or_insert_with(|| {
                crate::tor::Tor::new(config.clone(), self.config.data_dir.join("tor"))
            });
            tor.spawn_relay(addr, self.config.tls.clone(), self.net_tx.clone());
            return;
        }
        #[cfg(not(feature = "tor"))]
        if self.config.tor.is_some() {
            tracing::warn!("built without Tor support; connecting to the relay directly");
        }
        net::spawn(
            Link::Relay,
            addr,
            self.config.tls.clone(),
            self.net_tx.clone(),
        );
    }

    async fn feed(&mut self, input: Input) {
        let effects = self.core.handle(input, now_ms());
        self.apply(effects).await;
    }

    /// Executes effects in order. Pending persistence is committed before any
    /// frame leaves, so a crash can never replay ratchet keys or lose data the
    /// server already considers delivered.
    async fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Persist(op) => self.stage(op),
                Effect::Transmit(frame) => {
                    self.flush().await;
                    if let Some(gw) = &self.gateway {
                        let _ = gw.send(frame).await;
                    }
                }
                Effect::Anonymous(frame) => {
                    self.flush().await;
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
                } => match self.files.get(&file_id) {
                    Some(entry) => self.io.submit(IoJob::Read {
                        file_id,
                        path: entry.path.clone(),
                        index,
                        offset,
                        len,
                        headroom,
                    }),
                    None => {
                        let effects = self
                            .core
                            .handle(Input::ChunkUnavailable { file_id }, now_ms());
                        Box::pin(self.apply(effects)).await;
                    }
                },
                Effect::OpenSink {
                    file_id,
                    len,
                    sealed,
                } => {
                    // A sealed copy always lives at `media_path`; `files` keeps
                    // pointing at the transfer's plaintext source or target.
                    let path = if sealed {
                        self.config.media_path(&file_id)
                    } else if let Some(entry) = self.files.get(&file_id) {
                        entry.path.clone()
                    } else {
                        continue;
                    };
                    self.io.submit(IoJob::Open { file_id, path, len });
                }
                Effect::WriteChunk {
                    file_id,
                    offset,
                    data,
                } => {
                    self.io.submit(IoJob::Write {
                        file_id,
                        offset,
                        data,
                    });
                }
                Effect::CloseSink { file_id, complete } => {
                    self.io.submit(IoJob::Close {
                        file_id,
                        keep: complete,
                    });
                }
                Effect::Disconnect { reconnect } => {
                    self.reconnect = reconnect;
                    self.gateway = None;
                }
            }
        }
        self.flush().await;
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
        if let Event::TransferComplete { file_id } | Event::TransferFailed { file_id, .. } = &event
        {
            self.release_file(*file_id);
        }
        let _ = self.events.send(event);
    }

    fn stage(&mut self, op: StoreOp) {
        self.ops.push(op.into());
    }

    async fn flush(&mut self) {
        let ops = std::mem::take(&mut self.ops);
        if let Err(e) = self.store.apply(ops).await {
            tracing::error!("persisting client state failed: {e}");
        }
    }

    fn remember_file(&mut self, file_id: FileId, path: PathBuf) {
        let entry = FileEntry { path };
        let value = self.vault.seal_bytes(
            cypher_core::Table::Meta,
            &file_key(&file_id),
            &postcard::to_allocvec(&entry).expect("in-memory serialization"),
            &mut OsRng,
        );
        self.ops.push(Op::Put(FILES_TABLE, file_id.to_vec(), value));
        self.files.insert(file_id, entry);
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
