//! One client connection: authentication, routing and the writer task.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use cypher_server_kit::ratelimit::ConnLimiter;
use cypher_server_kit::{SIG_REQUEST_SUBJECT, peer_subject};
use cypher_types::{PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{
    ClientMsg, DeliveryStatus, ErrorCode, Frame, PROTOCOL_VERSION, ServerMsg, encode_recv,
    peek_send,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures::{Sink, SinkExt, Stream, StreamExt};
use rand::RngCore;
use tokio::sync::Semaphore;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::bus::{Bus, BusError, BusMsg};
use crate::metrics::Metrics;
use crate::outbox::{Outbox, OutboxReceiver, outbox};
use crate::registry::{ConnHandle, Registry};

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const PEER_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const SIG_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SIG_IN_FLIGHT: usize = 32;
const WRITE_BATCH: usize = 64;

const STATUS_DELIVERED: u8 = DeliveryStatus::Delivered as u8;
const STATUS_BUSY: u8 = DeliveryStatus::Busy as u8;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub frames_per_sec: u64,
    pub bytes_per_sec: u64,
}

pub struct Gateway<B> {
    pub bus: B,
    pub registry: Registry,
    pub limits: Limits,
    pub metrics: Metrics,
    next_conn: AtomicU64,
}

impl<B: Bus> Gateway<B> {
    pub fn new(bus: B, limits: Limits, metrics: Metrics) -> Self {
        Self {
            bus,
            registry: Registry::default(),
            limits,
            metrics,
            next_conn: AtomicU64::new(1),
        }
    }

    /// Drives one connection until it closes, whatever the transport.
    pub async fn handle<S, K>(self: Arc<Self>, stream: S, sink: K)
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
        K: Sink<Bytes, Error = io::Error> + Unpin + Send + 'static,
    {
        self.metrics.connections.inc();
        let (out, out_rx) = outbox();
        let cancel = CancellationToken::new();
        let writer = tokio::spawn(write_loop(sink, out_rx, cancel.clone()));
        let mut session = Session {
            gw: Arc::clone(&self),
            conn_id: self.next_conn.fetch_add(1, Ordering::Relaxed),
            out,
            cancel: cancel.clone(),
            limiter: ConnLimiter::new(self.limits.frames_per_sec, self.limits.bytes_per_sec),
            sig_permits: Arc::new(Semaphore::new(MAX_SIG_IN_FLIGHT)),
        };
        let peer = session.run(stream).await;
        if let Some(peer) = peer {
            self.registry.remove(&peer, session.conn_id);
        }
        cancel.cancel();
        let _ = writer.await;
        self.metrics.connections.dec();
    }
}

struct Session<B> {
    gw: Arc<Gateway<B>>,
    conn_id: u64,
    out: Outbox,
    cancel: CancellationToken,
    limiter: ConnLimiter,
    sig_permits: Arc<Semaphore>,
}

enum Next {
    Frame(Bytes),
    Relayed(BusMsg),
    /// The idle timer fired; the connection may have been active since.
    Idle,
    Kicked,
    Closed,
}

impl<B: Bus> Session<B> {
    /// Returns the authenticated peer (for registry cleanup), if any.
    async fn run<S>(&mut self, mut stream: S) -> Option<PeerId>
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
    {
        let peer = timeout(AUTH_TIMEOUT, self.authenticate(&mut stream))
            .await
            .ok()
            .flatten()?;
        if let Some((relayed, kick)) = self.attach(&peer).await {
            self.out.try_push(frame(0, ServerMsg::Ready));
            self.pump(&peer, stream, relayed, &kick).await;
        }
        Some(peer)
    }

    /// Registers the session, evicting older ones of the same identity here
    /// and on other nodes, and subscribes to frames relayed to it.
    async fn attach(&self, peer: &PeerId) -> Option<(B::Sub, CancellationToken)> {
        let kick = CancellationToken::new();
        if let Some(old) = self.gw.registry.insert(
            *peer,
            ConnHandle {
                conn_id: self.conn_id,
                outbox: self.out.clone(),
                kick: kick.clone(),
            },
        ) {
            old.outbox.try_push(frame(0, ServerMsg::Superseded));
            old.kick.cancel();
        }
        // An empty message evicts an older session on another node; it goes
        // out before this session subscribes, so it never reaches itself.
        let subject = peer_subject(&peer.to_hex());
        self.gw.bus.publish(subject.clone(), Bytes::new()).await;
        let relayed = self.gw.bus.subscribe(subject).await.ok()?;
        Some((relayed, kick))
    }

    /// Moves frames both ways until the client leaves, idles, is evicted or
    /// the gateway shuts down.
    async fn pump<S>(
        &mut self,
        peer: &PeerId,
        mut stream: S,
        mut relayed: B::Sub,
        kick: &CancellationToken,
    ) where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
    {
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
                    Some(msg) if msg.payload.is_empty() => {
                        self.out.try_push(frame(0, ServerMsg::Superseded));
                        Next::Kicked
                    }
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
                    if !self.on_frame(peer, bytes).await {
                        break;
                    }
                }
                Next::Relayed(msg) => self.on_relayed(msg).await,
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

    /// `Hello` → `Challenge` → `Auth` proof-of-possession of the identity key.
    async fn authenticate<S>(&self, stream: &mut S) -> Option<PeerId>
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
    {
        let hello = Frame::<ClientMsg>::decode(stream.next().await?.ok()?).ok()?;
        let ClientMsg::Hello { version, peer } = hello.msg else {
            return None;
        };
        if version != PROTOCOL_VERSION {
            self.out
                .try_push(frame(hello.req_id, error(ErrorCode::BadRequest)));
            return None;
        }
        let key = VerifyingKey::from_bytes(peer.as_bytes()).ok()?;
        let mut nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        self.out
            .try_push(frame(hello.req_id, ServerMsg::Challenge { nonce }));

        let auth = Frame::<ClientMsg>::decode(stream.next().await?.ok()?).ok()?;
        let ClientMsg::Auth { signature } = auth.msg else {
            return None;
        };
        let signed = [SESSION_AUTH_CONTEXT, nonce.as_slice()].concat();
        if key
            .verify(&signed, &Signature::from_bytes(&signature))
            .is_err()
        {
            self.gw.metrics.auth_failures.inc();
            self.out
                .try_push(frame(auth.req_id, error(ErrorCode::Unauthorized)));
            return None;
        }
        Some(peer)
    }

    /// Returns false when the connection must be closed.
    async fn on_frame(&mut self, me: &PeerId, bytes: Bytes) -> bool {
        self.gw.metrics.frames_in.inc();
        self.gw.metrics.bytes_in.inc_by(bytes.len() as u64);
        if !self.limiter.admit(bytes.len()) {
            self.gw.metrics.rate_limited.inc();
            return true;
        }
        if let Some((req_id, to, want_ack, body)) = peek_send(&bytes) {
            self.route(me, req_id, to, want_ack, &body).await;
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
            ClientMsg::Hello { .. } | ClientMsg::Auth { .. } | ClientMsg::Send { .. } => false,
            _ => {
                self.forward_to_signaling(*me, req_id, bytes);
                true
            }
        }
    }

    async fn route(&self, me: &PeerId, req_id: u32, to: PeerId, want_ack: bool, body: &[u8]) {
        if to == *me {
            return;
        }
        let relayed = encode_recv(me, body);
        if let Some(target) = self.gw.registry.get(&to) {
            self.gw.metrics.delivered_local.inc();
            let ok = target.outbox.try_push(relayed);
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
        let subject = peer_subject(&to.to_hex());
        if !want_ack {
            self.gw.bus.publish(subject, relayed).await;
            return;
        }
        let gw = Arc::clone(&self.gw);
        let out = self.out.clone();
        tokio::spawn(async move {
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
            self.gw
                .bus
                .publish(reply, Bytes::copy_from_slice(&[status]))
                .await;
        }
    }

    fn forward_to_signaling(&self, peer: PeerId, req_id: u32, request: Bytes) {
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
                    Some(peer),
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

async fn write_loop<K>(mut sink: K, mut rx: OutboxReceiver, cancel: CancellationToken)
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    loop {
        let first = tokio::select! {
            () = cancel.cancelled() => break,
            f = rx.recv() => match f {
                Some(f) => f,
                None => break,
            },
        };
        if sink.feed(first).await.is_err() {
            break;
        }
        for _ in 0..WRITE_BATCH {
            let Some(next) = rx.try_recv() else { break };
            if sink.feed(next).await.is_err() {
                cancel.cancel();
                return;
            }
        }
        if sink.flush().await.is_err() {
            break;
        }
    }
    while let Some(f) = rx.try_recv() {
        if sink.feed(f).await.is_err() {
            break;
        }
    }
    let _ = sink.close().await;
    cancel.cancel();
    debug!("writer closed");
}

fn frame(req_id: u32, msg: ServerMsg) -> Bytes {
    Frame::new(req_id, msg).encode()
}

fn error(code: ErrorCode) -> ServerMsg {
    ServerMsg::Error { code }
}
