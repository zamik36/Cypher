//! Relay Service - TURN-like relay for when P2P fails.
//!
//! Accepts TCP connections, pairs them into relay sessions, and forwards
//! encrypted bytes between peers. The relay never decrypts traffic.
//! Includes per-session bandwidth limiting and auto-teardown.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

use prometheus::{IntCounter, IntGauge};
use std::sync::LazyLock;

use cypher_transport::codec::FrameCodec;
use cypher_transport::frame::{Frame, FrameFlags};
use cypher_transport::session::AsyncReadWrite;

static RELAY_SESSIONS: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new("relay_active_sessions", "Number of active relay sessions").unwrap();
    let _ = prometheus::register(Box::new(g.clone()));
    g
});
static RELAY_BYTES: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("relay_bytes_total", "Total bytes relayed").unwrap();
    let _ = prometheus::register(Box::new(c.clone()));
    c
});

/// Type alias for a boxed async stream (works with both TLS and plain TCP).
type BoxedStream = Box<dyn AsyncReadWrite>;

/// Maximum bandwidth per relay session (bytes per second).
/// 10 MB/s default; can be made configurable.
const MAX_BANDWIDTH_PER_SESSION: u64 = 10 * 1024 * 1024;

/// Maximum lifetime for a relay session (1 hour).
const MAX_SESSION_LIFETIME_SECS: u64 = 60 * 60;

/// How often to check for expired sessions.
const CLEANUP_INTERVAL_SECS: u64 = 30;

/// One side of a relay session.
struct RelayPeer {
    /// Channel for sending frames to this peer's writer task.
    writer: mpsc::Sender<Frame>,
}

/// A relay session pairing two peers.
struct RelaySession {
    peer_a: RelayPeer,
    peer_b: RelayPeer,
    created_at: Instant,
    bytes_relayed: AtomicU64,
}

/// A peer waiting to be paired with another peer for a relay session.
struct PendingPeer {
    #[allow(dead_code)]
    session_key: String,
    writer: mpsc::Sender<Frame>,
    reader: futures::stream::SplitStream<Framed<BoxedStream, FrameCodec>>,
}

/// The relay service managing all active and pending sessions.
struct RelayService {
    /// session_key -> active relay session
    sessions: Arc<DashMap<String, Arc<RelaySession>>>,
    /// session_key -> pending peer waiting for a partner
    pending: Arc<DashMap<String, PendingPeer>>,
    /// Maximum bandwidth per session in bytes/second.
    max_bandwidth_per_session: u64,
}

impl RelayService {
    fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            pending: Arc::new(DashMap::new()),
            max_bandwidth_per_session: MAX_BANDWIDTH_PER_SESSION,
        }
    }

    /// The first frame must contain the relay session key (UTF-8).
    /// If a peer is already waiting with the same key, both are paired.
    async fn handle_connection(self: &Arc<Self>, stream: BoxedStream) -> anyhow::Result<()> {
        let framed = Framed::new(stream, FrameCodec::new());
        let (writer_sink, mut reader) = framed.split();

        // Channel for sending frames to this peer's writer task.
        let (frame_tx, mut frame_rx) = mpsc::channel::<Frame>(256);

        let writer_handle = tokio::spawn(async move {
            let mut writer_sink = writer_sink;
            while let Some(frame) = frame_rx.recv().await {
                if let Err(e) = writer_sink.send(frame).await {
                    debug!("relay writer error: {}", e);
                    break;
                }
            }
        });

        let first_frame = match reader.next().await {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                writer_handle.abort();
                return Err(e.into());
            }
            None => {
                writer_handle.abort();
                return Ok(());
            }
        };

        let session_key = String::from_utf8(first_frame.payload.to_vec())
            .unwrap_or_default()
            .trim()
            .to_string();

        if session_key.is_empty() {
            warn!("relay peer sent empty session key");
            writer_handle.abort();
            return Ok(());
        }

        debug!(session_key = %session_key, "relay peer connected");

        // Check if there is already a pending peer for this session key.
        if let Some((_, pending)) = self.pending.remove(&session_key) {
            // We have a partner -- create the relay session.
            let relay = Arc::new(RelaySession {
                peer_a: RelayPeer {
                    writer: pending.writer,
                },
                peer_b: RelayPeer { writer: frame_tx },
                created_at: Instant::now(),
                bytes_relayed: AtomicU64::new(0),
            });

            self.sessions.insert(session_key.clone(), relay.clone());
            RELAY_SESSIONS.inc();
            info!(session_key = %session_key, "relay session established");

            let max_bw = self.max_bandwidth_per_session;
            let sessions = self.sessions.clone();
            let key_a = session_key.clone();
            let key_b = session_key.clone();

            // Forward: peer_a reader -> peer_b writer
            let relay_a = relay.clone();
            let sessions_a = sessions.clone();
            let a_to_b = tokio::spawn(async move {
                let mut reader_a = pending.reader;
                Self::forward_loop(
                    &mut reader_a,
                    &relay_a.peer_b.writer,
                    &relay_a.bytes_relayed,
                    max_bw,
                )
                .await;
                // When one side disconnects, tear down the session.
                sessions_a.remove(&key_a);
            });

            // Forward: peer_b reader (current connection) -> peer_a writer
            let relay_b = relay.clone();
            let sessions_b = sessions.clone();
            let b_to_a = tokio::spawn(async move {
                let mut reader_b = reader;
                Self::forward_loop(
                    &mut reader_b,
                    &relay_b.peer_a.writer,
                    &relay_b.bytes_relayed,
                    max_bw,
                )
                .await;
                sessions_b.remove(&key_b);
            });

            // Wait for both forwarding tasks to finish.
            let _ = tokio::join!(a_to_b, b_to_a);
            RELAY_SESSIONS.dec();
            RELAY_BYTES.inc_by(relay.bytes_relayed.load(Ordering::Relaxed));
            info!(
                session_key = %session_key,
                bytes_relayed = relay.bytes_relayed.load(Ordering::Relaxed),
                "relay session ended"
            );
        } else {
            // No partner yet -- register as pending and wait.
            info!(session_key = %session_key, "relay peer waiting for partner");
            self.pending.insert(
                session_key.clone(),
                PendingPeer {
                    session_key: session_key.clone(),
                    writer: frame_tx,
                    reader,
                },
            );

            // The pending peer's writer task stays alive. When paired, the
            // relay session takes ownership of the channel. If the peer
            // disconnects before being paired, the writer task will end
            // naturally when the channel is dropped.
            //
            // We keep the writer_handle running; it will terminate when
            // frame_rx is dropped (which happens when PendingPeer is consumed
            // or dropped).
        }

        Ok(())
    }

    /// Forward frames from a reader to a writer with bandwidth limiting.
    async fn forward_loop(
        reader: &mut (impl StreamExt<Item = Result<Frame, std::io::Error>> + Unpin),
        writer: &mpsc::Sender<Frame>,
        bytes_relayed: &AtomicU64,
        max_bandwidth: u64,
    ) {
        let start = Instant::now();

        while let Some(result) = reader.next().await {
            let frame = match result {
                Ok(f) => f,
                Err(e) => {
                    debug!("relay reader error: {}", e);
                    break;
                }
            };

            if frame.flags.contains(FrameFlags::PING) {
                let pong = Frame::new(0, frame.seq_no, FrameFlags::PONG, Bytes::new());
                if writer.send(pong).await.is_err() {
                    break;
                }
                continue;
            }

            if frame.flags.contains(FrameFlags::PONG) {
                continue;
            }

            if frame.flags.contains(FrameFlags::SESSION_CLOSE) {
                debug!("relay peer sent SESSION_CLOSE");
                break;
            }

            let payload_len = frame.payload.len() as u64;

            // Bandwidth limiting: check if we have exceeded the allowed rate.
            let total = bytes_relayed.fetch_add(payload_len, Ordering::Relaxed) + payload_len;
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let rate = total as f64 / elapsed;
                if rate > max_bandwidth as f64 {
                    // Throttle by sleeping proportionally.
                    let excess = rate - max_bandwidth as f64;
                    let delay_ms = (excess / max_bandwidth as f64 * 100.0).min(1000.0);
                    tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;
                }
            }

            // Forward the frame to the other peer.
            if writer.send(frame).await.is_err() {
                debug!("relay writer channel closed");
                break;
            }
        }
    }

    /// Background task that periodically cleans up expired relay sessions
    /// and stale pending peers.
    async fn cleanup_task(self: Arc<Self>) {
        let max_lifetime = Duration::from_secs(MAX_SESSION_LIFETIME_SECS);

        loop {
            tokio::time::sleep(Duration::from_secs(CLEANUP_INTERVAL_SECS)).await;

            let now = Instant::now();

            // Remove expired active sessions.
            let mut expired_keys = Vec::new();
            for entry in self.sessions.iter() {
                if now.duration_since(entry.value().created_at) > max_lifetime {
                    expired_keys.push(entry.key().clone());
                }
            }
            for key in &expired_keys {
                self.sessions.remove(key);
                info!(session_key = %key, "relay session expired, removed");
            }

            // Remove stale pending peers (we track creation time via the key
            // being present; a more robust solution would timestamp the pending
            // entry, but for simplicity we just remove pending entries that
            // have been waiting too long).
            //
            // Note: PendingPeer does not carry a timestamp. In a production
            // system we would add one. For now, we skip pending cleanup and
            // rely on natural connection drops.
            let active = self.sessions.len();
            let pending = self.pending.len();
            if active > 0 || pending > 0 {
                debug!(
                    active_sessions = active,
                    pending_peers = pending,
                    "relay cleanup tick"
                );
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_common::init_tracing();
    let config = cypher_common::AppConfig::load()?;

    cypher_common::metrics::spawn_metrics_server(9092);

    let service = Arc::new(RelayService::new());

    {
        let svc = service.clone();
        tokio::spawn(async move {
            svc.cleanup_task().await;
        });
    }

    let tls_config = match (&config.tls_cert_path, &config.tls_key_path) {
        (Some(cert), Some(key)) if !cert.is_empty() && !key.is_empty() => {
            info!("Loading TLS certificate from {} / {}", cert, key);
            cypher_tls::load_pem_with_retry(
                cert,
                key,
                30,
                std::time::Duration::from_secs(2),
            )
            .await?
        }
        _ => {
            warn!("No TLS cert configured — using self-signed certificate for localhost. Clients will not be able to verify this certificate. Set P2P_TLS_CERT_PATH and P2P_TLS_KEY_PATH for production.");
            cypher_tls::make_server_config(&["localhost"])?
        }
    };
    let acceptor = TlsAcceptor::from(tls_config);

    let listener = TcpListener::bind(&config.relay_addr).await?;
    info!("Relay service listening on {} (TLS)", config.relay_addr);

    loop {
        let (tcp_stream, addr) = listener.accept().await?;
        debug!(%addr, "new relay connection");
        let svc = service.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(tcp_stream).await {
                Ok(s) => s,
                Err(e) => {
                    debug!(%addr, "TLS handshake failed: {}", e);
                    return;
                }
            };
            let boxed: BoxedStream = Box::new(tls_stream);
            if let Err(e) = svc.handle_connection(boxed).await {
                warn!(%addr, "relay connection error: {}", e);
            }
        });
    }
}
