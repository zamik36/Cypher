//! One client connection: authentication, routing and the writer task.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use cypher_server_kit::ratelimit::ConnLimiter;
use cypher_server_kit::{
    Origin, SEALED_REFUSED, SIG_REQUEST_SUBJECT, peer_subject, peer_subscription,
};
use cypher_types::{Addr, SESSION_AUTH_CONTEXT};
use cypher_wire::{
    ClientMsg, DeliveryStatus, ErrorCode, Frame, PROTOCOL_VERSION, SendView, ServerMsg,
    access_hash, encode_recv, encode_recv_sealed, peek_send,
};
use ed25519_dalek::{Signature, VerifyingKey};
use futures::{Sink, SinkExt, Stream, StreamExt};
use rand::RngCore;
use tokio::sync::Semaphore;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::bus::{Bus, BusError, BusMsg};
use crate::metrics::Metrics;
use crate::outbox::{Budget, Outbox, OutboxReceiver, Queued, Reservation, outbox};
use crate::registry::{ConnHandle, Registry};

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
    pub bus: B,
    pub registry: Registry,
    pub limits: Limits,
    pub metrics: Metrics,
    next_conn: AtomicU64,
    outbox_budget: Arc<Budget>,
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

    /// Drives one connection until it closes, whatever the transport.
    pub async fn handle<S, K>(self: Arc<Self>, stream: S, sink: K)
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
        K: Sink<Bytes, Error = io::Error> + Unpin + Send + 'static,
    {
        self.metrics.connections.inc();
        let (out, out_rx) = outbox(&self.outbox_budget);
        let cancel = CancellationToken::new();
        let mut writer = tokio::spawn(write_loop(sink, out_rx, cancel.clone()));
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

    /// `Hello` → `Challenge` → `Auth` proof-of-possession of the identity
    /// key, for the device the client names.
    async fn authenticate<S>(&self, stream: &mut S) -> Option<Addr>
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
    {
        let hello = Frame::<ClientMsg>::decode(stream.next().await?.ok()?).ok()?;
        let ClientMsg::Hello {
            version,
            peer,
            device,
        } = hello.msg
        else {
            return None;
        };
        if version != PROTOCOL_VERSION {
            self.out
                .try_push(frame(hello.req_id, error(ErrorCode::UnsupportedVersion)));
            return None;
        }
        let mut nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        self.out
            .try_push(frame(hello.req_id, ServerMsg::Challenge { nonce }));

        let auth = Frame::<ClientMsg>::decode(stream.next().await?.ok()?).ok()?;
        let ClientMsg::Auth { signature } = auth.msg else {
            return None;
        };
        let signed = [
            SESSION_AUTH_CONTEXT,
            nonce.as_slice(),
            &device.0.to_le_bytes(),
        ]
        .concat();
        // Strict: a small-order key would otherwise "sign" any challenge.
        let verified = VerifyingKey::from_bytes(peer.as_bytes())
            .and_then(|key| key.verify_strict(&signed, &Signature::from_bytes(&signature)));
        if verified.is_err() {
            self.gw.metrics.auth_failures.inc();
            self.out
                .try_push(frame(auth.req_id, error(ErrorCode::Unauthorized)));
            return None;
        }
        Some(Addr::new(peer, device))
    }

    /// Returns false when the connection must be closed.
    async fn on_frame(&mut self, me: Addr, bytes: Bytes) -> bool {
        self.gw.metrics.frames_in.inc();
        self.gw.metrics.bytes_in.inc_by(bytes.len() as u64);
        if !self.limiter.admit(bytes.len()) {
            self.gw.metrics.rate_limited.inc();
            self.notice_rate_limited(&bytes);
            return true;
        }
        if let Some(send) = peek_send(&bytes) {
            self.route(me, send).await;
            return true;
        }
        let Ok(Frame { req_id, msg }) = Frame::<ClientMsg>::decode(bytes.clone()) else {
            return false;
        };
        match msg {
            ClientMsg::Ping => {
                self.out.try_push(frame(req_id, ServerMsg::Pong));
                true
            }
            ClientMsg::SetAccess { hashes } => {
                self.access = hashes;
                self.out.try_push(frame(req_id, ServerMsg::Done));
                true
            }
            ClientMsg::Hello { .. } | ClientMsg::Auth { .. } | ClientMsg::Send { .. } => false,
            _ => {
                self.forward_to_signaling(me, req_id, bytes);
                true
            }
        }
    }

    /// Answers a frame dropped by the rate limiter with `RateLimited`, so
    /// the client need not wait for its request to time out. At most once
    /// per `LIMIT_NOTICE_INTERVAL`: answering every dropped frame would let
    /// a flood make the gateway write as much as it reads.
    fn notice_rate_limited(&mut self, dropped: &Bytes) {
        let now = Instant::now();
        if now < self.next_limit_notice {
            return;
        }
        let req_id = match peek_send(dropped) {
            Some(send) => send.req_id,
            None => match Frame::<ClientMsg>::decode(dropped.clone()) {
                Ok(frame) => frame.req_id,
                Err(_) => return,
            },
        };
        self.next_limit_notice = now + LIMIT_NOTICE_INTERVAL;
        self.out
            .try_push(frame(req_id, error(ErrorCode::RateLimited)));
    }

    /// Relays a `Send` to the device it names; a device may write to its
    /// siblings, never to itself.
    async fn route(&self, me: Addr, send: SendView) {
        let SendView {
            req_id,
            to,
            device,
            want_ack,
            body,
        } = send;
        let to = Addr::new(to, device);
        if to == me {
            return;
        }
        let relayed = encode_recv(&me.peer, me.device, &body);
        if let Some(target) = self.gw.registry.get(&to) {
            let ok = target.outbox.try_push(relayed);
            if ok {
                self.gw.metrics.delivered_local.inc();
            } else {
                self.gw.metrics.busy.inc();
            }
            if want_ack {
                let status = if ok {
                    DeliveryStatus::Delivered
                } else {
                    DeliveryStatus::Busy
                };
                self.ack(req_id, status);
            }
            return;
        }
        let subject = peer_subject(&to.peer.to_hex(), to.device, Origin::Gateway);
        if !want_ack {
            self.publish(subject, relayed).await;
            return;
        }
        let Ok(permit) = Arc::clone(&self.ack_permits).try_acquire_owned() else {
            self.gw.metrics.busy.inc();
            self.ack(req_id, DeliveryStatus::Busy);
            return;
        };
        let gw = Arc::clone(&self.gw);
        let out = self.out.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let status = match gw
                .bus
                .request(subject, None, relayed, PEER_REQUEST_TIMEOUT)
                .await
            {
                Ok(reply) if reply.first() == Some(&STATUS_DELIVERED) => {
                    gw.metrics.delivered_remote.inc();
                    DeliveryStatus::Delivered
                }
                Ok(_) | Err(BusError::Unavailable) => DeliveryStatus::Busy,
                Err(BusError::NoResponders | BusError::Timeout) => DeliveryStatus::Offline,
            };
            out.try_push(frame(req_id, ServerMsg::SendAck { status }));
        });
    }

    fn ack(&self, req_id: u32, status: DeliveryStatus) {
        self.out
            .try_push(frame(req_id, ServerMsg::SendAck { status }));
    }

    /// A frame another node relayed to this peer.
    async fn on_relayed(&self, msg: BusMsg) {
        let ok = self.out.try_push(msg.payload);
        if !ok {
            self.gw.metrics.busy.inc();
        }
        if let Some(reply) = msg.reply {
            let status = if ok { STATUS_DELIVERED } else { STATUS_BUSY };
            self.publish(reply, Bytes::copy_from_slice(&[status])).await;
        }
    }

    /// A delivery signaling relays without a sender (`token ‖ item`): the
    /// device gets it only with a token it told this gateway, so signaling
    /// cannot write to devices that did not hand their contacts one.
    async fn on_sealed(&self, msg: BusMsg) {
        let status = match msg.payload.split_first_chunk::<32>() {
            Some((token, item)) if knows(&self.access, &access_hash(token)) => {
                if self.out.try_push(encode_recv_sealed(item)) {
                    self.gw.metrics.sealed_delivered.inc();
                    STATUS_DELIVERED
                } else {
                    self.gw.metrics.busy.inc();
                    STATUS_BUSY
                }
            }
            _ => {
                self.gw.metrics.sealed_refused.inc();
                SEALED_REFUSED
            }
        };
        if let Some(reply) = msg.reply {
            self.publish(reply, Bytes::copy_from_slice(&[status])).await;
        }
    }

    /// Publishes on the bus, where nothing waits for an answer: a failure
    /// is only counted. False when the bus did not take it.
    async fn publish(&self, subject: String, payload: Bytes) -> bool {
        let Err(error) = self.gw.bus.publish(subject, payload).await else {
            return true;
        };
        self.gw.metrics.publish_failed.inc();
        debug!(?error, "bus publish failed");
        false
    }

    fn forward_to_signaling(&self, me: Addr, req_id: u32, request: Bytes) {
        let Ok(permit) = Arc::clone(&self.sig_permits).try_acquire_owned() else {
            self.out
                .try_push(frame(req_id, error(ErrorCode::RateLimited)));
            return;
        };
        let gw = Arc::clone(&self.gw);
        let out = self.out.clone();
        tokio::spawn(async move {
            let reply = gw
                .bus
                .request(
                    SIG_REQUEST_SUBJECT.to_owned(),
                    Some(me.peer),
                    request,
                    SIG_REQUEST_TIMEOUT,
                )
                .await
                .unwrap_or_else(|_| frame(req_id, error(ErrorCode::Unavailable)));
            out.try_push(reply);
            drop(permit);
        });
    }
}

/// Writes the outbox to the client until the session ends, then delivers
/// what is left and closes. A failed or stalled write ends the session.
async fn write_loop<K>(mut sink: K, mut rx: OutboxReceiver, cancel: CancellationToken)
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    if write_until_closed(&mut sink, &mut rx, &cancel).await {
        drain_and_close(&mut sink, &mut rx).await;
    }
    cancel.cancel();
    debug!("writer closed");
}

/// Writes what is still queued and closes; a failed write leaves the rest,
/// and closing, undone.
async fn drain_and_close<K>(sink: &mut K, rx: &mut OutboxReceiver)
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    let mut held = None;
    while let Some(queued) = rx.try_recv() {
        if !feed(sink, queued, &mut held).await {
            return;
        }
    }
    written(sink.close()).await;
}

/// Writes queued frames in batches, one flush each. True when the session
/// ended, false when a write failed or stalled.
async fn write_until_closed<K>(
    sink: &mut K,
    rx: &mut OutboxReceiver,
    cancel: &CancellationToken,
) -> bool
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    loop {
        let first = tokio::select! {
            () = cancel.cancelled() => return true,
            f = rx.recv() => match f {
                Some(f) => f,
                None => return true,
            },
        };
        // The batch's bytes stay counted until it is flushed.
        let mut held = None;
        if !feed(sink, first, &mut held).await {
            return false;
        }
        for _ in 0..WRITE_BATCH {
            let Some(next) = rx.try_recv() else { break };
            if !feed(sink, next, &mut held).await {
                return false;
            }
        }
        if !written(sink.flush()).await {
            return false;
        }
    }
}

/// Writes a queued frame, adding its bytes to those `held` until the next
/// flush.
async fn feed<K>(sink: &mut K, queued: Queued, held: &mut Option<Reservation>) -> bool
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    let (frame, reservation) = queued.into_parts();
    match held {
        Some(held) => held.merge(reservation),
        None => *held = Some(reservation),
    }
    written(sink.feed(frame)).await
}

/// Runs one write to the client for at most `WRITE_TIMEOUT`; false when it
/// failed or stalled.
async fn written(write: impl Future<Output = io::Result<()>>) -> bool {
    matches!(timeout(WRITE_TIMEOUT, write).await, Ok(Ok(())))
}

/// Whether `msg` announces a session of the device signing in on some node:
/// an empty message on its gateway subject, where relayed frames never are.
fn is_eviction(msg: &BusMsg) -> bool {
    msg.origin == Some(Origin::Gateway) && msg.payload.is_empty()
}

/// Whether `hash` is one of `known`, comparing every byte of each.
fn knows(known: &[[u8; 32]], hash: &[u8; 32]) -> bool {
    let matched = known.iter().fold(0u8, |matched, k| {
        let diff = k.iter().zip(hash).fold(0u8, |acc, (a, b)| acc | (a ^ b));
        matched | u8::from(diff == 0)
    });
    matched != 0
}

fn frame(req_id: u32, msg: ServerMsg) -> Bytes {
    Frame::new(req_id, msg).encode()
}

fn error(code: ErrorCode) -> ServerMsg {
    ServerMsg::Error { code }
}
