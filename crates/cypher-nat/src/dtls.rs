//! Secure UDP framing for the P2P channel.
//!
//! After UDP hole punching succeeds, this layer wraps the raw socket with:
//! - **HMAC-SHA256 authentication** (derived from X3DH shared secret)
//! - **Sequence numbers** for replay protection
//! - **Simple handshake** to verify both sides share the secret
//!
//! This complements (not replaces) the E2EE Double Ratchet layer: DTLS-like
//! transport security prevents IP spoofing and replay at the network level,
//! while Double Ratchet provides end-to-end confidentiality.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use hmac::{Hmac, Mac};
use cypher_common::{Error, Result};
use sha2::Sha256;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Maximum UDP datagram payload (excluding our header).
const MAX_PAYLOAD: usize = 65000;

/// Header layout:
/// ```text
/// [0..8]   seq_no   (u64 LE)
/// [8..40]  hmac     (32 bytes HMAC-SHA256)
/// [40..]   payload
/// ```
const HEADER_LEN: usize = 8 + 32; // seq_no + HMAC

/// Handshake magic for connection verification.
const HANDSHAKE_MAGIC: &[u8; 8] = b"P2P-DTLS";
const HANDSHAKE_ACK: &[u8; 8] = b"P2P-DACK";

/// A secure UDP session over a hole-punched connection.
///
/// Both peers must share the same `session_key` (derived from X3DH).
/// The session provides:
/// - Authentication via HMAC-SHA256 on every datagram
/// - Replay protection via monotonic sequence numbers
/// - A simple handshake to verify key agreement
pub struct DtlsSession {
    socket: Arc<UdpSocket>,
    remote: SocketAddr,
    send_seq: AtomicU64,
    recv_seq: AtomicU64,
    hmac_key: [u8; 32],
}

impl DtlsSession {
    /// Perform a secure handshake as the **initiator** (link creator).
    ///
    /// Sends a HMAC'd handshake message and waits for an ack.
    pub async fn connect_as_client(
        socket: Arc<UdpSocket>,
        remote: SocketAddr,
        session_key: &[u8; 32],
    ) -> Result<Self> {
        let session = Self {
            socket,
            remote,
            send_seq: AtomicU64::new(1),
            recv_seq: AtomicU64::new(0),
            hmac_key: *session_key,
        };

        // Send handshake.
        session.send_authenticated(HANDSHAKE_MAGIC).await?;
        debug!("DTLS handshake sent to {}", remote);

        // Wait for ack (with timeout).
        let mut buf = vec![0u8; HEADER_LEN + HANDSHAKE_ACK.len()];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, session.socket.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) if from == remote => {
                    if let Some(payload) = session.verify_and_strip(&buf[..n]) {
                        if payload == HANDSHAKE_ACK {
                            info!("DTLS handshake completed (client)");
                            return Ok(session);
                        }
                    }
                }
                Ok(Ok(_)) => continue, // wrong source
                Ok(Err(e)) => return Err(Error::Transport(format!("DTLS handshake recv: {e}"))),
                Err(_) => return Err(Error::Transport("DTLS handshake timeout".into())),
            }
        }
    }

    /// Perform a secure handshake as the **responder** (link joiner).
    ///
    /// Waits for a handshake message and sends an ack.
    pub async fn accept_as_server(
        socket: Arc<UdpSocket>,
        remote: SocketAddr,
        session_key: &[u8; 32],
    ) -> Result<Self> {
        let session = Self {
            socket,
            remote,
            send_seq: AtomicU64::new(1),
            recv_seq: AtomicU64::new(0),
            hmac_key: *session_key,
        };

        // Wait for handshake.
        let mut buf = vec![0u8; HEADER_LEN + HANDSHAKE_MAGIC.len()];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, session.socket.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) if from == remote => {
                    if let Some(payload) = session.verify_and_strip(&buf[..n]) {
                        if payload == HANDSHAKE_MAGIC {
                            // Send ack.
                            session.send_authenticated(HANDSHAKE_ACK).await?;
                            info!("DTLS handshake completed (server)");
                            return Ok(session);
                        }
                    }
                }
                Ok(Ok(_)) => continue,
                Ok(Err(e)) => return Err(Error::Transport(format!("DTLS handshake recv: {e}"))),
                Err(_) => return Err(Error::Transport("DTLS handshake timeout".into())),
            }
        }
    }

    /// Send authenticated data to the remote peer.
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        if data.len() > MAX_PAYLOAD {
            return Err(Error::Transport("payload too large for UDP".into()));
        }
        self.send_authenticated(data).await
    }

    /// Receive authenticated data from the remote peer.
    ///
    /// Returns `None` on timeout (no data within 30s).
    pub async fn recv(&self) -> Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; HEADER_LEN + MAX_PAYLOAD];
        let timeout = std::time::Duration::from_secs(30);

        loop {
            match tokio::time::timeout(timeout, self.socket.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) => {
                    if from != self.remote {
                        continue; // ignore packets from other addresses
                    }
                    match self.verify_and_strip(&buf[..n]) {
                        Some(payload) => return Ok(Some(payload.to_vec())),
                        None => {
                            warn!("received unauthenticated/replayed packet from {}", from);
                            continue;
                        }
                    }
                }
                Ok(Err(e)) => return Err(Error::Transport(format!("DTLS recv: {e}"))),
                Err(_) => return Ok(None), // timeout
            }
        }
    }

    /// Return the remote address.
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote
    }

    /// Return the underlying socket.
    pub fn socket(&self) -> &Arc<UdpSocket> {
        &self.socket
    }

    async fn send_authenticated(&self, payload: &[u8]) -> Result<()> {
        let seq = self.send_seq.fetch_add(1, Ordering::Relaxed);

        // Build: [seq_no LE 8B] [placeholder HMAC 32B] [payload]
        let total = HEADER_LEN + payload.len();
        let mut buf = Vec::with_capacity(total);
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.extend_from_slice(&[0u8; 32]); // placeholder HMAC
        buf.extend_from_slice(payload);

        // Compute HMAC over [seq_no + payload] (not the placeholder).
        let mut mac = HmacSha256::new_from_slice(&self.hmac_key)
            .map_err(|e| Error::Crypto(format!("HMAC init: {e}")))?;
        mac.update(&buf[..8]); // seq_no
        mac.update(&buf[HEADER_LEN..]); // payload
        let hmac_bytes = mac.finalize().into_bytes();
        buf[8..40].copy_from_slice(&hmac_bytes);

        self.socket
            .send_to(&buf, self.remote)
            .await
            .map_err(|e| Error::Transport(format!("DTLS send: {e}")))?;
        Ok(())
    }

    /// Verify HMAC and check sequence number. Returns payload on success.
    fn verify_and_strip<'a>(&self, data: &'a [u8]) -> Option<&'a [u8]> {
        if data.len() < HEADER_LEN {
            return None;
        }

        let seq_bytes: [u8; 8] = data[..8].try_into().ok()?;
        let seq = u64::from_le_bytes(seq_bytes);
        let received_hmac = &data[8..40];
        let payload = &data[HEADER_LEN..];

        // Compute expected HMAC.
        let mut mac = HmacSha256::new_from_slice(&self.hmac_key).ok()?;
        mac.update(&data[..8]); // seq_no
        mac.update(payload);
        let expected = mac.finalize().into_bytes();

        // Constant-time comparison.
        if !constant_time_eq(received_hmac, &expected) {
            return None;
        }

        // Replay protection: sequence must be greater than last received.
        let prev = self.recv_seq.load(Ordering::Relaxed);
        if seq <= prev {
            debug!(seq, prev, "replayed or out-of-order DTLS packet");
            return None;
        }
        self.recv_seq.store(seq, Ordering::Relaxed);

        Some(payload)
    }
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dtls_handshake_and_exchange() {
        let key = [42u8; 32];

        let sock_a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sock_b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());

        let addr_a = sock_a.local_addr().unwrap();
        let addr_b = sock_b.local_addr().unwrap();

        let client_task = tokio::spawn({
            let sock = Arc::clone(&sock_a);
            async move { DtlsSession::connect_as_client(sock, addr_b, &key).await }
        });

        let server_task = tokio::spawn({
            let sock = Arc::clone(&sock_b);
            async move { DtlsSession::accept_as_server(sock, addr_a, &key).await }
        });

        let (client_result, server_result) = tokio::join!(client_task, server_task);
        let client = client_result.unwrap().unwrap();
        let server = server_result.unwrap().unwrap();

        // Send from client to server.
        client.send(b"hello from client").await.unwrap();
        let msg = server.recv().await.unwrap().unwrap();
        assert_eq!(msg, b"hello from client");

        // Send from server to client.
        server.send(b"hello from server").await.unwrap();
        let msg = client.recv().await.unwrap().unwrap();
        assert_eq!(msg, b"hello from server");
    }
}
