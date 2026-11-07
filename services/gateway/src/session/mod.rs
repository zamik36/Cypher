//! One client connection: authentication, routing and the writer task.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use cypher_server_kit::ratelimit::ConnLimiter;
use cypher_server_kit::{Origin, peer_subject, peer_subscription};
use cypher_types::Addr;
use cypher_wire::{DeliveryStatus, ErrorCode, Frame, ServerMsg};
use futures::{Sink, Stream, StreamExt};
use rand::RngCore;
use tokio::sync::Semaphore;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::bus::{Bus, BusMsg};
use crate::metrics::Metrics;
use crate::outbox::{Budget, Outbox, outbox};
use crate::registry::{ConnHandle, Registry};

mod auth;
mod route;
mod writer;

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one write to a client (a frame, a flush, the close) may take.
/// The transport flushes about a frame per write, so a client must take a
/// full-size frame (1 MiB) at some 35 KB/s; one that stops reading loses
/// the session instead of holding its connection, queue and address slot.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a closing session lets its writer deliver the last frames
/// (`Superseded`, an error) before abandoning it. Frames it has not
/// written by then are dropped unsent; the client learns nothing of them.
pub(crate) const WRITER_GRACE: Duration = Duration::from_secs(5);
const PEER_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const SIG_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SIG_IN_FLIGHT: usize = 32;
/// Acknowledged sends to peers on other nodes awaiting their answer, per
/// connection. Each holds its frame (up to 1 MiB) for up to
/// `PEER_REQUEST_TIMEOUT`; beyond the limit the sender is told `Busy` at once.
const MAX_ACKS_IN_FLIGHT: usize = 32;
const WRITE_BATCH: usize = 64;
/// How often a client is told that frames over its rate limit were dropped.
pub(crate) const LIMIT_NOTICE_INTERVAL: Duration = Duration::from_secs(1);

/// What a session announces itself by when it signs in, evicting the
/// device's sessions on other nodes.
type EvictNonce = u128;
/// How long a session waits to hear its own announcement back before it
/// takes any other as newer (see `Echo`); the bus returns it in a round
/// trip, milliseconds.
const ECHO_WAIT: Duration = Duration::from_secs(5);

const STATUS_DELIVERED: u8 = DeliveryStatus::Delivered as u8;
const STATUS_BUSY: u8 = DeliveryStatus::Busy as u8;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub frames_per_sec: u64,
    pub bytes_per_sec: u64,
    /// Bytes the queues of all connections may hold together.
    pub outbox_bytes: usize,
}

pub struct Gateway<B> {
    bus: B,
    registry: Registry,
    limits: Limits,
    metrics: Metrics,
    next_conn: AtomicU64,
    outbox_budget: Arc<Budget>,
}

impl<B> std::fmt::Debug for Gateway<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gateway(..)")
    }
}

impl<B: Bus> Gateway<B> {
    pub fn new(bus: B, limits: Limits, metrics: Metrics) -> Self {
        Self {
            bus,
            registry: Registry::default(),
            limits,
            metrics,
            next_conn: AtomicU64::new(1),
            outbox_budget: Budget::new(limits.outbox_bytes),
        }
    }

    pub(crate) fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    #[cfg(test)]
    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Drives one connection until it closes, whatever the transport.
    pub async fn handle<S, K>(self: Arc<Self>, stream: S, sink: K)
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
        K: Sink<Bytes, Error = io::Error> + Unpin + Send + 'static,
    {
        self.metrics.connections.inc();
        let (out, out_rx) = outbox(&self.outbox_budget);
        let cancel = CancellationToken::new();
        let mut writer = tokio::spawn(writer::write_loop(sink, out_rx, cancel.clone()));
        let mut session = Session {
            gw: Arc::clone(&self),
            conn_id: self.next_conn.fetch_add(1, Ordering::Relaxed),
            out,
            cancel: cancel.clone(),
            limiter: ConnLimiter::new(self.limits.frames_per_sec, self.limits.bytes_per_sec),
            next_limit_notice: Instant::now(),
            sig_permits: Arc::new(Semaphore::new(MAX_SIG_IN_FLIGHT)),
            ack_permits: Arc::new(Semaphore::new(MAX_ACKS_IN_FLIGHT)),
            access: Vec::new(),
        };
        if let Some(me) = session.run(stream).await {
            self.registry.remove(&me, session.conn_id);
        }
        cancel.cancel();
        if timeout(WRITER_GRACE, &mut writer).await.is_err() {
            writer.abort();
        }
        self.metrics.connections.dec();
    }

    /// Bytes queued for all clients right now.
    #[cfg(test)]
    pub(crate) fn outbox_used(&self) -> usize {
        self.outbox_budget.used()
    }
}

struct Session<B> {
    gw: Arc<Gateway<B>>,
    conn_id: u64,
    out: Outbox,
    cancel: CancellationToken,
    limiter: ConnLimiter,
    /// When a dropped frame may next be answered with `RateLimited`.
    next_limit_notice: Instant,
    sig_permits: Arc<Semaphore>,
    ack_permits: Arc<Semaphore>,
    /// Hashes of the tokens the device takes sealed deliveries with.
    access: Vec<[u8; 32]>,
}

/// A signed-in session's subscription and announcement.
struct Attached<S> {
    relayed: S,
    kick: CancellationToken,
    nonce: EvictNonce,
    echo: Echo,
}

/// Whether a session has heard its own announcement back, which decides
/// which of two logins of a device racing on two nodes stays. The bus
/// hands announcements on one subject to every subscriber in the same
/// order, and both sessions subscribed before announcing. Another
/// session's announcement heard before this one's own was sent earlier:
/// that session hears this one's afterwards and yields to it. One heard
/// after is a newer login, which this session yields to.
enum Echo {
    /// Not heard back yet; announcements arriving until `until` are older.
    /// The bound covers an announcement lost on the way.
    Awaited { until: Instant },
    /// Heard back, or never sent: every other announcement is newer.
    Heard,
}

impl Echo {
    /// Whether this session yields to another's announcement arriving at
    /// `now`.
    fn yields(&self, now: Instant) -> bool {
        match self {
            Self::Awaited { until } => now >= *until,
            Self::Heard => true,
        }
    }
}

enum Next {
    Frame(Bytes),
    Relayed(BusMsg),
    /// A session of the device signed in somewhere, this one included; the
    /// nonce it announced, if its gateway names one.
    Announcement(Option<EvictNonce>),
    /// The idle timer fired; the connection may have been active since.
    Idle,
    Kicked,
    Closed,
}

impl<B: Bus> Session<B> {
    /// Returns the authenticated device (for registry cleanup), if any.
    async fn run<S>(&mut self, mut stream: S) -> Option<Addr>
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
    {
        let me = timeout(AUTH_TIMEOUT, self.authenticate(&mut stream))
            .await
            .ok()
            .flatten()?;
        let mut nonce = [0; 16];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let nonce = EvictNonce::from_le_bytes(nonce);
        if let Some(attached) = self.attach(me, nonce).await {
            self.out.try_push(frame(0, ServerMsg::Ready));
            self.pump(me, stream, attached).await;
        }
        Some(me)
    }

    /// Registers the session, evicting older ones of the same device here
    /// and on other nodes, and subscribes to frames relayed to it. Other
    /// devices of the identity stay connected.
    async fn attach(&self, me: Addr, nonce: EvictNonce) -> Option<Attached<B::Sub>> {
        let kick = CancellationToken::new();
        if let Some(old) = self.gw.registry.insert(
            me,
            ConnHandle {
                conn_id: self.conn_id,
                outbox: self.out.clone(),
                kick: kick.clone(),
            },
        ) {
            old.outbox.try_push(frame(0, ServerMsg::Superseded));
            old.kick.cancel();
        }
        // The announcement evicts the device's sessions on other nodes.
        // Subscribing first means of two logins racing on two nodes at least
        // one hears the other; see `Echo` for which of them stays.
        let hex = me.peer.to_hex();
        let relayed = self
            .gw
            .bus
            .subscribe(peer_subscription(&hex, me.device))
            .await
            .ok()?;
        let evict = peer_subject(&hex, me.device, Origin::Gateway);
        let echo = match self.gw.bus.announce(evict, nonce).await {
            Ok(()) => Echo::Awaited {
                until: Instant::now() + ECHO_WAIT,
            },
            Err(error) => {
                self.gw.metrics.publish_failed.inc();
                warn!(
                    ?error,
                    "eviction not published: the device may stay signed in on another node"
                );
                Echo::Heard
            }
        };
        Some(Attached {
            relayed,
            kick,
            nonce,
            echo,
        })
    }

    /// Moves frames both ways until the client leaves, idles, is evicted or
    /// the gateway shuts down.
    async fn pump<S>(&mut self, me: Addr, mut stream: S, attached: Attached<B::Sub>)
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
    {
        let Attached {
            mut relayed,
            kick,
            nonce,
            mut echo,
        } = attached;
        // One timer per session, re-armed only when it fires: resetting it on
        // every frame put a timer-wheel insert and removal on the hot path.
        let idle = sleep(IDLE_TIMEOUT);
        tokio::pin!(idle);
        let mut last_frame = Instant::now();
        loop {
            let next = tokio::select! {
                biased;
                () = self.cancel.cancelled() => Next::Closed,
                () = kick.cancelled() => Next::Kicked,
                msg = relayed.next() => match msg {
                    Some(msg) if is_eviction(&msg) => Next::Announcement(msg.announcer),
                    Some(msg) => Next::Relayed(msg),
                    None => Next::Closed,
                },
                item = stream.next() => match item {
                    Some(Ok(bytes)) => Next::Frame(bytes),
                    _ => Next::Closed,
                },
                () = &mut idle => Next::Idle,
            };
            match next {
                Next::Frame(bytes) => {
                    last_frame = Instant::now();
                    if !self.on_frame(me, bytes).await {
                        break;
                    }
                }
                Next::Relayed(msg) => match msg.origin {
                    Some(Origin::Gateway) => self.on_relayed(msg).await,
                    Some(Origin::Sealed) => self.on_sealed(msg).await,
                    None => {}
                },
                Next::Announcement(announcer) if announcer == Some(nonce) => echo = Echo::Heard,
                Next::Announcement(_) => {
                    if echo.yields(Instant::now()) {
                        self.out.try_push(frame(0, ServerMsg::Superseded));
                        break;
                    }
                }
                Next::Idle => {
                    let deadline = last_frame + IDLE_TIMEOUT;
                    if deadline <= Instant::now() {
                        break;
                    }
                    idle.as_mut().reset(deadline);
                }
                Next::Kicked | Next::Closed => break,
            }
        }
    }
}

/// Whether `msg` announces a session of the device signing in on some node:
/// an empty message on its gateway subject, where relayed frames never are.
fn is_eviction(msg: &BusMsg) -> bool {
    msg.origin == Some(Origin::Gateway) && msg.payload.is_empty()
}

fn frame(req_id: u32, msg: ServerMsg) -> Bytes {
    Frame::new(req_id, msg).encode()
}

fn error(code: ErrorCode) -> ServerMsg {
    ServerMsg::Error { code }
}
