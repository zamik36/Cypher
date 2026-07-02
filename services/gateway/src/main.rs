//! Gateway Service - TLS connection manager.
//!
//! Accepts TLS connections via [`cypher_transport::TransportListener`], manages
//! sessions, and routes frames between clients and the signaling service via
//! NATS.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;
use tracing::{debug, error, info, warn};

use prometheus::{IntCounter, IntGauge};
use std::sync::LazyLock;

use cypher_common::ratelimit::TokenBucket;
use cypher_common::{
    Error as P2pError, HEARTBEAT_INTERVAL_SECS, MAX_MISSED_HEARTBEATS, MSG_RATE_LIMIT_BURST,
    MSG_RATE_LIMIT_PER_SEC,
};

static ACTIVE_CONNECTIONS: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new(
        "gateway_active_connections",
        "Number of active client connections",
    )
    .unwrap();
    let _ = prometheus::register(Box::new(g.clone()));
    g
});
static MESSAGES_ROUTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new(
        "gateway_messages_routed_total",
        "Total messages routed through gateway",
    )
    .unwrap();
    let _ = prometheus::register(Box::new(c.clone()));
    c
});
static BYTES_RELAYED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new(
        "gateway_bytes_relayed_total",
        "Total bytes relayed through gateway",
    )
    .unwrap();
    let _ = prometheus::register(Box::new(c.clone()));
    c
});
static CONNECTIONS_REJECTED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new(
        "gateway_connections_rejected_total",
        "Connections dropped because the connection cap was reached",
    )
    .unwrap();
    let _ = prometheus::register(Box::new(c.clone()));
    c
});
use cypher_proto::{dispatch, Message, Serializable};
use cypher_transport::frame::{Frame, FrameFlags};
use cypher_transport::{TransportListener, TransportSession};

/// State for a single connected peer.
///
/// The `writer` channel carries `(payload, flags)` pairs rather than full
/// [`Frame`] values.  This decouples [`ConnectionState`] from sequence-number
/// management (handled inside [`TransportSession`]) and lets the heartbeat task
/// inject PING frames without constructing a frame directly.
struct ConnectionState {
    session_id: u64,
    /// Authenticated peer identity. Empty until the client proves possession of
    /// the private key via SESSION_AUTH; routing only ever uses this field.
    peer_id: Vec<u8>,
    /// Peer identity claimed in SESSION_INIT but not yet proven (challenge issued).
    pending_peer_id: Vec<u8>,
    /// The `server_nonce` challenge issued in SESSION_INIT, awaiting a signature.
    server_nonce: Vec<u8>,
    addr: SocketAddr,
    /// Channel for enqueueing outbound frames to this peer's session task.
    writer: mpsc::Sender<(Bytes, FrameFlags)>,
    /// Timestamp of the last received frame (for heartbeat tracking).
    last_activity: Instant,
}

/// The gateway server holding all active connections.
struct Gateway {
    /// session_id → connection state
    connections: Arc<DashMap<u64, ConnectionState>>,
    /// peer_id bytes → session_id (for routing by peer identity)
    peers: Arc<DashMap<Vec<u8>, u64>>,
    /// Monotonically increasing session counter.
    next_session_id: AtomicU64,
    /// NATS client for communicating with the signaling service.
    nats: async_nats::Client,
    /// Maximum number of concurrent connections; excess are dropped at accept.
    max_connections: usize,
    /// This gateway's unique node id; namespaces NATS session subjects so
    /// session ids never collide across replicas.
    node_id: String,
}

impl Gateway {
    async fn new(
        nats_url: &str,
        nats_token: Option<&str>,
        max_connections: usize,
        node_id: String,
    ) -> anyhow::Result<Self> {
        let nats = match nats_token {
            Some(token) if !token.is_empty() => {
                async_nats::ConnectOptions::with_token(token.to_string())
                    .connect(nats_url)
                    .await?
            }
            _ => async_nats::connect(nats_url).await?,
        };
        Ok(Self {
            connections: Arc::new(DashMap::new()),
            peers: Arc::new(DashMap::new()),
            next_session_id: AtomicU64::new(1),
            nats,
            max_connections,
            node_id,
        })
    }

    /// Whether the connection cap has been reached. Uses the active-connections
    /// gauge as a coarse (racy but safe) bound to prevent unbounded task/memory
    /// growth under connection floods.
    fn at_capacity(&self) -> bool {
        ACTIVE_CONNECTIONS.get() as usize >= self.max_connections
    }

    /// Allocate a new unique session ID.
    fn allocate_session_id(&self) -> u64 {
        self.next_session_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Main accept loop.  Hands each incoming [`TransportSession`] off to its
    /// own task and registers it in the connection map.
    async fn accept_loop(self: Arc<Self>, mut listener: TransportListener) {
        loop {
            match listener.accept().await {
                Ok(session) => {
                    // Shed load past the connection cap: drop the freshly accepted
                    // session instead of spawning unbounded tasks/memory.
                    if self.at_capacity() {
                        CONNECTIONS_REJECTED.inc();
                        warn!(
                            max = self.max_connections,
                            "connection cap reached, dropping new TLS session"
                        );
                        drop(session);
                        continue;
                    }
                    // TransportListener does not expose the peer address after
                    // the TLS handshake; use a zeroed placeholder.  Session IDs
                    // are the primary identifier at this layer.
                    let addr: SocketAddr = "0.0.0.0:0".parse().expect("static addr");
                    let gw = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = gw.handle_session(session, addr).await {
                            debug!("session error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("accept error: {}", e);
                    // Brief pause to avoid a tight error loop on persistent
                    // accept failures (e.g. too many open files).
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    ///
    /// Ownership model:
    /// - [`TransportSession`] is moved into `spawn_session_task`, which runs
    ///   a `tokio::select!` loop owning the socket for both reads and writes.
    /// - Inbound frames are forwarded to this task via `inbound_rx`.
    /// - Outbound frames are sent to the session task via `frame_tx`.
    async fn handle_session(
        self: Arc<Self>,
        session: TransportSession,
        addr: SocketAddr,
    ) -> anyhow::Result<()> {
        let session_id = self.allocate_session_id();
        ACTIVE_CONNECTIONS.inc();
        info!(session_id, %addr, "new TLS session");

        // Channel: this task → session task (outbound frames).
        let (frame_tx, frame_rx) = mpsc::channel::<(Bytes, FrameFlags)>(256);
        // Channel: session task → this task (inbound frames).
        let (inbound_tx, mut inbound_rx) = mpsc::channel::<Frame>(256);

        // Register the connection (peer_id empty until SESSION_INIT).
        self.connections.insert(
            session_id,
            ConnectionState {
                session_id,
                peer_id: Vec::new(),
                pending_peer_id: Vec::new(),
                server_nonce: Vec::new(),
                addr,
                writer: frame_tx.clone(),
                last_activity: Instant::now(),
            },
        );

        // Session owner task: exclusively owns the TransportSession and bridges
        // the two mpsc channels to/from the socket.
        let session_handle = Self::spawn_session_task(session, frame_rx, inbound_tx);

        // NATS → peer forwarding task.
        let nats_subject = cypher_common::gateway_session_subject(&self.node_id, session_id);
        let mut nats_sub = self.nats.subscribe(nats_subject).await?;
        let nats_frame_tx = frame_tx.clone();
        let nats_handle = tokio::spawn(async move {
            while let Some(msg) = nats_sub.next().await {
                let payload = Bytes::from(msg.payload.to_vec());
                if nats_frame_tx
                    .send((payload, FrameFlags::NONE))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Read loop: process inbound frames forwarded by the session task.
        let result = self.read_loop(session_id, &mut inbound_rx, &frame_tx).await;

        // Cleanup on disconnect.
        self.remove_connection(session_id).await;
        session_handle.abort();
        nats_handle.abort();

        match result {
            Ok(()) => debug!(session_id, "session closed gracefully"),
            Err(ref e) => debug!(session_id, "session closed: {}", e),
        }

        Ok(())
    }

    /// Spawn a task that exclusively owns a [`TransportSession`].
    ///
    /// The task runs a `tokio::select!` loop that simultaneously:
    /// - drains `frame_rx` → writes frames to the socket via `send_frame`
    /// - reads frames from the socket via `recv_frame` → sends them on `inbound_tx`
    ///
    /// `TransportSession` wraps `Box<dyn AsyncReadWrite>` where `AsyncReadWrite`
    /// requires `Send`, so the session is `Send` and can be moved into a
    /// `tokio::spawn` task without restriction.
    fn spawn_session_task(
        mut session: TransportSession,
        mut frame_rx: mpsc::Receiver<(Bytes, FrameFlags)>,
        inbound_tx: mpsc::Sender<Frame>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Outbound: write an enqueued frame to the socket.
                    msg = frame_rx.recv() => {
                        match msg {
                            Some((payload, flags)) => {
                                if let Err(e) = session.send_frame(payload, flags).await {
                                    debug!("session write error: {}", e);
                                    break;
                                }
                            }
                            None => {
                                // All senders dropped; close the connection gracefully.
                                let _ = session.close().await;
                                break;
                            }
                        }
                    }

                    // Inbound: forward a received frame to the read loop.
                    frame_result = session.recv_frame() => {
                        match frame_result {
                            Ok(frame) => {
                                if inbound_tx.send(frame).await.is_err() {
                                    // Read loop dropped its receiver; exit.
                                    break;
                                }
                            }
                            Err(P2pError::ConnectionClosed) => break,
                            Err(e) => {
                                debug!("session read error: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
        })
    }

    /// Main read loop for a connection.
    ///
    /// Receives decoded frames from the session task via `inbound_rx` and
    /// dispatches them: control frames are handled inline; data frames are
    /// routed to NATS or directly to another peer.
    async fn read_loop(
        &self,
        session_id: u64,
        inbound_rx: &mut mpsc::Receiver<Frame>,
        frame_tx: &mpsc::Sender<(Bytes, FrameFlags)>,
    ) -> anyhow::Result<()> {
        let mut rate_limiter =
            TokenBucket::new(MSG_RATE_LIMIT_BURST, MSG_RATE_LIMIT_PER_SEC as f64);

        while let Some(frame) = inbound_rx.recv().await {
            // Refresh the last-activity timestamp on every received frame.
            if let Some(mut conn) = self.connections.get_mut(&session_id) {
                conn.last_activity = Instant::now();
            }

            // --- Control frames (exempt from rate limiting) ---
            if frame.flags.contains(FrameFlags::PING) {
                debug!(session_id, "received PING, sending PONG");
                let _ = frame_tx.send((Bytes::new(), FrameFlags::PONG)).await;
                continue;
            }

            if frame.flags.contains(FrameFlags::PONG) {
                debug!(session_id, "received PONG");
                continue;
            }

            if frame.flags.contains(FrameFlags::SESSION_CLOSE) {
                info!(session_id, "client sent SESSION_CLOSE");
                return Ok(());
            }

            if frame.flags.contains(FrameFlags::SESSION_INIT) {
                self.handle_session_init(session_id, &frame.payload, frame_tx)
                    .await?;
                continue;
            }

            // --- Per-session rate limiting for data frames ---
            if !rate_limiter.try_consume(1) {
                warn!(session_id, "rate limit exceeded, dropping frame");
                continue;
            }

            // --- Data frames: route via NATS / direct forwarding ---
            MESSAGES_ROUTED.inc();
            BYTES_RELAYED.inc_by(frame.payload.len() as u64);
            self.route_frame(session_id, &frame).await?;
        }

        // Channel closed → session task has exited.
        Ok(())
    }

    /// Process a SESSION_INIT-flagged frame.
    ///
    /// Two-step, mutually-authenticated handshake:
    /// 1. `SessionInit` — the client *claims* a peer_id. We issue a random
    ///    `server_nonce` challenge and reply with a `SessionAck`, but register
    ///    nothing yet.
    /// 2. `SessionAuth` — the client returns an Ed25519 signature over
    ///    `SESSION_AUTH_CONTEXT || server_nonce`. Only after verifying it against
    ///    the claimed peer_id (which *is* the Ed25519 public key) do we register
    ///    the peer for routing. This proves possession of the private key and
    ///    stops anyone from claiming another user's identity (C2).
    async fn handle_session_init(
        &self,
        session_id: u64,
        payload: &[u8],
        frame_tx: &mpsc::Sender<(Bytes, FrameFlags)>,
    ) -> anyhow::Result<()> {
        let now_secs = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        };

        match dispatch(payload) {
            Ok(Message::SessionInit(init)) => {
                let peer_id = init.client_id.clone();
                if peer_id.len() != 32 {
                    warn!(session_id, len = peer_id.len(), "SESSION_INIT bad peer_id");
                    anyhow::bail!("session init: peer_id must be 32 bytes");
                }
                info!(session_id, "session init: issuing auth challenge");

                // Issue the challenge; stash it as *pending* — no routing identity yet.
                let mut server_nonce = vec![0u8; 32];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut server_nonce);
                if let Some(mut conn) = self.connections.get_mut(&session_id) {
                    conn.pending_peer_id = peer_id;
                    conn.server_nonce = server_nonce.clone();
                }

                let ack = cypher_proto::SessionAck {
                    server_nonce,
                    timestamp: now_secs(),
                };
                let _ = frame_tx
                    .send((Bytes::from(ack.serialize()), FrameFlags::SESSION_INIT))
                    .await;
            }
            Ok(Message::SessionAuth(auth)) => {
                // Pull the pending challenge without holding the map guard across await.
                let (pending_peer_id, server_nonce) = match self.connections.get(&session_id) {
                    Some(conn) => (conn.pending_peer_id.clone(), conn.server_nonce.clone()),
                    None => anyhow::bail!("session auth: unknown session"),
                };
                if pending_peer_id.is_empty() || server_nonce.is_empty() {
                    warn!(session_id, "SESSION_AUTH before SESSION_INIT");
                    anyhow::bail!("session auth: no pending challenge");
                }

                // Verify the proof-of-possession.
                let vk_bytes: [u8; 32] = pending_peer_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("session auth: bad peer_id length"))?;
                let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&vk_bytes)
                    .map_err(|e| anyhow::anyhow!("session auth: invalid peer_id key: {e}"))?;
                let sig_bytes: [u8; 64] = auth
                    .signature
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("session auth: signature must be 64 bytes"))?;
                let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);

                let mut signed = cypher_common::SESSION_AUTH_CONTEXT.to_vec();
                signed.extend_from_slice(&server_nonce);
                if ed25519_dalek::Verifier::verify(&verifying_key, &signed, &signature).is_err() {
                    warn!(session_id, "SESSION_AUTH signature verification failed");
                    anyhow::bail!("session auth: signature verification failed");
                }

                // Authenticated — promote the pending identity to the routing map.
                if let Some(mut conn) = self.connections.get_mut(&session_id) {
                    conn.peer_id = pending_peer_id.clone();
                    conn.pending_peer_id = Vec::new();
                    conn.server_nonce = Vec::new();
                }
                self.peers.insert(pending_peer_id.clone(), session_id);

                let session_info = serde_json::json!({
                    "session_id": session_id,
                    "peer_id": hex_encode(&pending_peer_id),
                    "gateway_node": self.node_id,
                });
                self.nats
                    .publish(
                        "signaling.session.register".to_string(),
                        Bytes::from(session_info.to_string()),
                    )
                    .await?;
                info!(session_id, "session authenticated and registered");

                // Confirm with an empty-nonce ack so the client can proceed.
                let ack = cypher_proto::SessionAck {
                    server_nonce: Vec::new(),
                    timestamp: now_secs(),
                };
                let _ = frame_tx
                    .send((Bytes::from(ack.serialize()), FrameFlags::SESSION_INIT))
                    .await;
            }
            _ => {
                warn!(session_id, "invalid SESSION_INIT payload");
                anyhow::bail!("invalid SESSION_INIT payload");
            }
        }
        Ok(())
    }

    /// Try to deliver `frame` directly to a locally-connected peer; if not
    /// found, return `(subject, Some(peer_id))` so the caller can publish to
    /// NATS and let the signaling service handle the cross-node forward.
    async fn try_direct_or_subject(
        &self,
        peer_id: &[u8],
        frame: &Frame,
        subject: &str,
    ) -> (String, Option<Vec<u8>>) {
        if let Some(target_session) = self.peers.get(peer_id) {
            if let Some(conn) = self.connections.get(target_session.value()) {
                let _ = conn
                    .writer
                    .send((frame.payload.clone(), FrameFlags::NONE))
                    .await;
                // Signal caller that direct delivery was done; skip NATS publish.
                return ("".to_string(), None);
            }
        }
        (subject.to_string(), Some(peer_id.to_vec()))
    }

    /// Route a data frame from a client to the signaling service via NATS, or
    /// directly to another peer on this gateway when possible.
    async fn route_frame(&self, session_id: u64, frame: &Frame) -> anyhow::Result<()> {
        if frame.payload.is_empty() {
            return Ok(());
        }

        // JSON messages (e.g. create_link) are not binary proto — route them
        // by the "action" field before attempting proto dispatch.
        if frame.payload.first() == Some(&b'{') {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&frame.payload) {
                let action = json.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let subject = match action {
                    "create_link" => "signaling.create_link",
                    _ => "signaling.raw",
                };

                let conn_entry = self.connections.get(&session_id);
                let peer_id_hex = conn_entry
                    .as_ref()
                    .map(|c| hex_encode(&c.peer_id))
                    .unwrap_or_default();

                info!(
                    session_id,
                    action,
                    subject,
                    conn_found = conn_entry.is_some(),
                    peer_id_len = conn_entry.as_ref().map(|c| c.peer_id.len()).unwrap_or(0),
                    "routing JSON message"
                );
                drop(conn_entry);

                let envelope = serde_json::json!({
                    "session_id": session_id,
                    "peer_id": peer_id_hex,
                    "gateway_node": self.node_id,
                });
                self.nats
                    .publish(subject.to_string(), Bytes::from(envelope.to_string()))
                    .await?;

                MESSAGES_ROUTED.inc();
                return Ok(());
            }
        }

        match dispatch(&frame.payload) {
            Ok(ref msg) => {
                let (subject, _target_peer) = match msg {
                    Message::SignalRequestPeer(_) => ("signaling.request_peer".to_string(), None),
                    Message::SignalIceCandidate(ice) => (
                        "signaling.ice_candidate".to_string(),
                        Some(ice.peer_id.clone()),
                    ),
                    Message::SignalOffer(offer) => {
                        ("signaling.offer".to_string(), Some(offer.peer_id.clone()))
                    }
                    Message::SignalAnswer(answer) => {
                        ("signaling.answer".to_string(), Some(answer.peer_id.clone()))
                    }
                    Message::KeysUploadPrekeys(_) => ("signaling.upload_prekeys".to_string(), None),
                    Message::KeysGetPrekeys(_) => ("signaling.get_prekeys".to_string(), None),
                    Message::TransportBootstrap(_) => {
                        ("signaling.transport_bootstrap".to_string(), None)
                    }
                    Message::InboxStore(_) => ("signaling.inbox_store".to_string(), None),
                    Message::InboxFetch(_) => ("signaling.inbox_fetch".to_string(), None),
                    Message::InboxAck(_) => ("signaling.inbox_ack".to_string(), None),
                    Message::ChatSend(ref chat) => {
                        let target_peer_id = chat.peer_id.clone();

                        // Rewrite peer_id from target → sender so the receiver
                        // knows who the message is from.
                        let sender_peer_id = self
                            .connections
                            .get(&session_id)
                            .map(|c| c.peer_id.clone())
                            .unwrap_or_default();
                        let rewritten_chat = cypher_proto::ChatSend {
                            peer_id: sender_peer_id,
                            ciphertext: chat.ciphertext.clone(),
                            ratchet_key: chat.ratchet_key.clone(),
                            msg_no: chat.msg_no,
                        };
                        let rewritten = Bytes::from(rewritten_chat.serialize());

                        // Attempt direct peer-to-peer routing within this gateway.
                        if let Some(target_session) = self.peers.get(&target_peer_id) {
                            if let Some(conn) = self.connections.get(target_session.value()) {
                                let _ = conn.writer.send((rewritten, FrameFlags::NONE)).await;
                                return Ok(());
                            }
                        }
                        ("signaling.chat_send".to_string(), Some(target_peer_id))
                    }
                    // File transfer messages: route directly if the peer is local,
                    // else forward via signaling (same pattern as ChatSend).
                    Message::FileOffer(m) => {
                        self.try_direct_or_subject(&m.peer_id, frame, "signaling.file_offer")
                            .await
                    }
                    Message::FileAccept(m) => {
                        self.try_direct_or_subject(&m.peer_id, frame, "signaling.file_accept")
                            .await
                    }
                    Message::FileChunk(m) => {
                        self.try_direct_or_subject(&m.peer_id, frame, "signaling.file_chunk")
                            .await
                    }
                    Message::FileComplete(m) => {
                        self.try_direct_or_subject(&m.peer_id, frame, "signaling.file_complete")
                            .await
                    }
                    Message::FileChunkAck(m) => {
                        self.try_direct_or_subject(&m.peer_id, frame, "signaling.file_chunk_ack")
                            .await
                    }
                    Message::FileResume(m) => {
                        self.try_direct_or_subject(&m.peer_id, frame, "signaling.file_resume")
                            .await
                    }
                    _ => ("signaling.data".to_string(), None),
                };

                // Empty subject means direct delivery was already done; skip NATS.
                if subject.is_empty() {
                    return Ok(());
                }

                // Route via signaling. Local direct-delivery is handled inline by
                // the ChatSend / try_direct_or_subject arms above; signaling owns
                // the peer_id rewrite for ICE/offer/answer, so we must NOT also
                // forward the raw frame here — doing so double-delivered those
                // messages (and with an un-rewritten peer_id).
                //
                // Binary envelope (node_id + session_id prefix + raw payload)
                // avoids the ~4-6x bloat of JSON-encoding the payload as an array
                // of numbers, and carries our node so signaling can reply to us.
                let envelope = cypher_common::GatewayEnvelope::encode(
                    &self.node_id,
                    session_id,
                    &frame.payload,
                );
                self.nats.publish(subject, Bytes::from(envelope)).await?;
            }
            Err(e) => {
                debug!(session_id, "could not dispatch frame payload: {}", e);
                // Forward raw payload to signaling so nothing is silently dropped.
                let envelope = cypher_common::GatewayEnvelope::encode(
                    &self.node_id,
                    session_id,
                    &frame.payload,
                );
                self.nats
                    .publish("signaling.raw".to_string(), Bytes::from(envelope))
                    .await?;
            }
        }

        Ok(())
    }

    /// Gracefully drain all active sessions on shutdown: deregister each peer
    /// from signaling and close its socket, so no phantom sessions linger in
    /// Redis until TTL and peers see the disconnect promptly.
    async fn drain_sessions(&self) {
        let session_ids: Vec<u64> = self.connections.iter().map(|e| *e.key()).collect();
        info!(count = session_ids.len(), "draining active sessions");
        for session_id in session_ids {
            self.remove_connection(session_id).await;
        }
    }

    /// Remove a connection and clean up the associated peer mapping.
    async fn remove_connection(&self, session_id: u64) {
        if let Some((_, conn)) = self.connections.remove(&session_id) {
            ACTIVE_CONNECTIONS.dec();
            info!(session_id, %conn.addr, "removing connection");
            if !conn.peer_id.is_empty() {
                self.peers
                    .remove_if(&conn.peer_id, |_, sid| *sid == session_id);
                let dereg = serde_json::json!({
                    "session_id": session_id,
                });
                if let Err(error) = self
                    .nats
                    .publish(
                        "signaling.session.deregister".to_string(),
                        Bytes::from(dereg.to_string()),
                    )
                    .await
                {
                    warn!(
                        session_id,
                        "failed to publish session deregister: {}", error
                    );
                }
            }
        }
    }

    /// Accept WebSocket connections from browser-based (PWA) clients.
    ///
    /// Each WS binary message maps directly to a proto payload — no custom
    /// framing is needed because WebSocket already provides message boundaries.
    async fn ws_accept_loop(self: Arc<Self>, addr: SocketAddr) {
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("failed to bind WebSocket listener on {}: {}", addr, e);
                return;
            }
        };
        info!("Gateway (WS) listening on {}", addr);

        loop {
            let (stream, peer_addr) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    error!("WS accept error: {}", e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            // Shed load past the connection cap before the WS handshake cost.
            if self.at_capacity() {
                CONNECTIONS_REJECTED.inc();
                warn!(
                    max = self.max_connections,
                    %peer_addr, "connection cap reached, dropping new WS session"
                );
                drop(stream);
                continue;
            }
            let gw = self.clone();
            tokio::spawn(async move {
                match tokio_tungstenite::accept_async(stream).await {
                    Ok(ws) => {
                        if let Err(e) = gw.handle_ws_session(ws, peer_addr).await {
                            debug!(%peer_addr, "WS session error: {}", e);
                        }
                    }
                    Err(e) => {
                        debug!(%peer_addr, "WS handshake error: {}", e);
                    }
                }
            });
        }
    }

    async fn handle_ws_session(
        self: Arc<Self>,
        ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        addr: SocketAddr,
    ) -> anyhow::Result<()> {
        let session_id = self.allocate_session_id();
        ACTIVE_CONNECTIONS.inc();
        info!(session_id, %addr, "new WS session");

        let (mut ws_sink, mut ws_stream) = ws.split();

        // Channel: outbound frames to send over WS.
        let (frame_tx, mut frame_rx) = mpsc::channel::<(Bytes, FrameFlags)>(256);

        // Register connection.
        self.connections.insert(
            session_id,
            ConnectionState {
                session_id,
                peer_id: Vec::new(),
                pending_peer_id: Vec::new(),
                server_nonce: Vec::new(),
                addr,
                writer: frame_tx.clone(),
                last_activity: Instant::now(),
            },
        );

        // NATS → WS forwarding task.
        let nats_subject = cypher_common::gateway_session_subject(&self.node_id, session_id);
        let mut nats_sub = self.nats.subscribe(nats_subject).await?;
        let nats_frame_tx = frame_tx.clone();
        let nats_handle = tokio::spawn(async move {
            while let Some(msg) = nats_sub.next().await {
                let payload = Bytes::from(msg.payload.to_vec());
                if nats_frame_tx
                    .send((payload, FrameFlags::NONE))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // WS writer task: drain frame_rx → send as binary WS messages.
        let writer_handle = tokio::spawn(async move {
            while let Some((payload, _flags)) = frame_rx.recv().await {
                let msg = tungstenite::Message::Binary(payload.to_vec());
                if ws_sink.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = ws_sink.close().await;
        });

        // WS reader: convert incoming binary messages to Frame-like processing.
        let (inbound_tx, mut inbound_rx) = mpsc::channel::<Frame>(256);
        let reader_handle = tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_stream.next().await {
                match msg {
                    tungstenite::Message::Binary(data) => {
                        let frame = Frame {
                            seq_no: 0,
                            ack: 0,
                            flags: FrameFlags::NONE,
                            payload: Bytes::from(data.to_vec()),
                        };
                        if data.len() >= 4 {
                            let cid = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                            // SESSION_INIT (0xA1000001) and SESSION_AUTH (0xA1000003)
                            // both drive the handshake and must carry the flag so the
                            // read loop routes them to handle_session_init.
                            if cid == 0xA1000001 || cid == 0xA1000003 {
                                let mut init_frame = frame.clone();
                                init_frame.flags = FrameFlags::SESSION_INIT;
                                if inbound_tx.send(init_frame).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                        }
                        if inbound_tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    tungstenite::Message::Close(_) => break,
                    tungstenite::Message::Ping(data) => {
                        // Pong is handled automatically by tungstenite.
                        let _ = data;
                    }
                    _ => {} // Ignore text, pong, etc.
                }
            }
        });

        // Reuse the same read_loop that TLS sessions use.
        let result = self.read_loop(session_id, &mut inbound_rx, &frame_tx).await;

        // Cleanup.
        self.remove_connection(session_id).await;
        reader_handle.abort();
        writer_handle.abort();
        nats_handle.abort();

        match result {
            Ok(()) => debug!(session_id, "WS session closed gracefully"),
            Err(ref e) => debug!(session_id, "WS session closed: {}", e),
        }

        Ok(())
    }

    /// Background task that periodically checks all connections for liveness.
    ///
    /// Sends PING frames to idle connections and removes peers that have not
    /// responded within the allowed number of missed heartbeats.
    async fn heartbeat_task(self: Arc<Self>) {
        let interval = Duration::from_secs(HEARTBEAT_INTERVAL_SECS);
        let timeout = interval * MAX_MISSED_HEARTBEATS;

        loop {
            tokio::time::sleep(interval).await;

            let now = Instant::now();
            let mut stale = Vec::new();

            for entry in self.connections.iter() {
                let conn = entry.value();
                let elapsed = now.duration_since(conn.last_activity);

                if elapsed > timeout {
                    warn!(
                        session_id = conn.session_id,
                        %conn.addr,
                        "heartbeat timeout, marking for removal"
                    );
                    stale.push(conn.session_id);
                } else if elapsed > interval {
                    // Probe with a PING; drop silently if the channel is full.
                    let _ = conn.writer.try_send((Bytes::new(), FrameFlags::PING));
                }
            }

            for session_id in stale {
                self.remove_connection(session_id).await;
            }
        }
    }
}

/// Encode a byte slice as a lowercase hex string.
///
/// Avoids pulling in an external `hex` crate for this single use.
fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_common::init_tracing();
    let config = cypher_common::AppConfig::load()?;

    cypher_common::metrics::spawn_metrics_server(9090);

    let tls_config = match (&config.tls_cert_path, &config.tls_key_path) {
        (Some(cert), Some(key)) if !cert.is_empty() && !key.is_empty() => {
            info!("Loading TLS certificate from {} / {}", cert, key);
            cypher_tls::load_pem_with_retry(cert, key, 30, std::time::Duration::from_secs(2))
                .await?
        }
        _ => {
            warn!("No TLS cert configured — using self-signed certificate for localhost. Clients will not be able to verify this certificate. Set P2P_TLS_CERT_PATH and P2P_TLS_KEY_PATH for production.");
            cypher_tls::make_server_config(&["localhost"])?
        }
    };

    let nats_token = std::env::var("P2P_NATS_TOKEN").ok();
    info!(
        node_id = %config.node_id,
        max_connections = config.max_connections,
        "gateway node configured"
    );
    let gateway = Arc::new(
        Gateway::new(
            &config.nats_url,
            nats_token.as_deref(),
            config.max_connections,
            config.node_id.clone(),
        )
        .await?,
    );

    {
        let gw = gateway.clone();
        tokio::spawn(async move {
            gw.heartbeat_task().await;
        });
    }

    {
        let gw = gateway.clone();
        let ws_addr: SocketAddr = config.ws_addr.parse()?;
        tokio::spawn(async move {
            gw.ws_accept_loop(ws_addr).await;
        });
    }

    let listener = TransportListener::bind(&config.gateway_addr, tls_config).await?;
    info!("Gateway (TLS) listening on {}", config.gateway_addr);

    tokio::select! {
        _ = gateway.clone().accept_loop(listener) => {}
        _ = cypher_common::shutdown_signal() => {
            info!("shutdown signal received; draining sessions");
            gateway.drain_sessions().await;
            info!("gateway shutdown complete");
        }
    }

    Ok(())
}
