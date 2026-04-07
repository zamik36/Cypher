use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use cypher_common::{Error, PeerId, Result};
use cypher_crypto::identity::IdentityKeyPair;
use cypher_transport::{Frame, FrameFlags};
use tracing::{debug, info};

use crate::connection::ServerConnection;
use crate::signaling::SignalingClient;

const MAX_REQUESTS: u32 = 30;
const MAX_TTL: Duration = Duration::from_secs(10 * 60); // 10 minutes
const INIT_TIMEOUT: Duration = Duration::from_secs(10);

/// An ephemeral, unlinkable gateway session for anonymous operations.
///
/// Shadow sessions use a freshly generated identity (peer_id) and do NOT
/// upload prekeys, making them invisible to the signaling discovery system.
/// Used by both Tier 1 (Tor) and Tier 2 (Relay) anonymous transports.
pub struct ShadowSession {
    conn: ServerConnection,
    peer_id: PeerId,
    created_at: Instant,
    request_count: u32,
}

impl ShadowSession {
    /// Open a new shadow session to the gateway at `addr`.
    ///
    /// 1. Generates a fresh ephemeral `IdentityKeyPair`
    /// 2. Opens a TLS connection via `ServerConnection::connect_tls`
    /// 3. Sends `SESSION_INIT` with the ephemeral peer_id
    /// 4. Does **not** upload prekeys (shadow identity is not discoverable)
    pub async fn connect(addr: &str, tls_config: Arc<rustls::ClientConfig>) -> Result<Self> {
        let conn = ServerConnection::connect_tls(addr, tls_config).await?;
        Self::connect_with_connection(conn).await
    }

    pub(crate) async fn connect_with_connection(conn: ServerConnection) -> Result<Self> {
        let identity = IdentityKeyPair::generate();
        let peer_id = identity.peer_id();
        debug!(%peer_id, "shadow session: connecting to gateway");
        let mut signaling = SignalingClient::new(conn);

        let nonce: [u8; 32] = rand::random();
        match tokio::time::timeout(
            INIT_TIMEOUT,
            signaling.session_init(peer_id.to_vec(), nonce.to_vec()),
        )
        .await
        {
            Ok(Ok(_)) => info!(%peer_id, "shadow session: SESSION_INIT completed"),
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(Error::Transport(
                    "shadow session: SESSION_INIT timed out".into(),
                ))
            }
        }

        Ok(Self {
            conn: signaling.conn,
            peer_id,
            created_at: Instant::now(),
            request_count: 0,
        })
    }

    /// Send a payload and wait for the server's response frame.
    pub async fn send_and_recv(&mut self, payload: Bytes, flags: FrameFlags) -> Result<Frame> {
        if self.is_expired() {
            return Err(Error::Transport("shadow session expired".into()));
        }
        self.conn.send_payload(payload, flags).await?;
        self.request_count += 1;
        self.conn.recv_frame().await
    }

    /// Send a payload without waiting for a response.
    pub async fn send(&mut self, payload: Bytes, flags: FrameFlags) -> Result<()> {
        if self.is_expired() {
            return Err(Error::Transport("shadow session expired".into()));
        }
        self.conn.send_payload(payload, flags).await?;
        self.request_count += 1;
        Ok(())
    }

    /// Receive the next frame from the server.
    pub async fn recv(&mut self) -> Result<Frame> {
        self.conn.recv_frame().await
    }

    /// Whether this session has exceeded its lifetime or request budget.
    pub fn is_expired(&self) -> bool {
        self.request_count >= MAX_REQUESTS || self.created_at.elapsed() > MAX_TTL
    }

    /// Remaining requests before expiry.
    pub fn remaining_requests(&self) -> u32 {
        MAX_REQUESTS.saturating_sub(self.request_count)
    }

    /// The ephemeral peer_id for this shadow session.
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    /// Gracefully close the session.
    pub async fn close(mut self) -> Result<()> {
        debug!(peer_id = %self.peer_id, "shadow session: closing");
        self.conn.close().await
    }
}
