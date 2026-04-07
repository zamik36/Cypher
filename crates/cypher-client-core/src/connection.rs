use std::sync::Arc;

use bytes::Bytes;
use tokio::net::TcpStream;

use cypher_common::{Error, Result};
use cypher_transport::{Frame, FrameFlags, TransportSession};

/// Manages the TCP connection to the gateway server.
///
/// Wraps [`TransportSession`] and provides a clean, high-level async API:
///
/// - [`connect_plain`](ServerConnection::connect_plain) — open an unencrypted
///   TCP connection (development / local-gateway mode).
/// - [`send_payload`](ServerConnection::send_payload) — send an arbitrary byte
///   payload wrapped in a transport frame.
/// - [`recv_frame`](ServerConnection::recv_frame) — receive the next frame from
///   the server.
/// - [`close`](ServerConnection::close) — gracefully shut down the session.
///
/// In production, swap `connect_plain` for a TLS-aware constructor that passes
/// an `Arc<rustls::ClientConfig>` to `TransportSession::connect`.
pub struct ServerConnection {
    session: TransportSession,
}

impl ServerConnection {
    /// Wrap an existing transport session.
    pub fn from_session(session: TransportSession) -> Self {
        Self { session }
    }

    /// Open a TLS connection to `addr` using the given [`rustls::ClientConfig`].
    ///
    /// This is the production path. For development / local-gateway, use
    /// [`connect_plain`](ServerConnection::connect_plain) instead.
    pub async fn connect_tls(addr: &str, tls_config: Arc<rustls::ClientConfig>) -> Result<Self> {
        let session = TransportSession::connect(addr, tls_config).await?;
        tracing::debug!(addr, "TLS connection established");
        Ok(Self { session })
    }

    /// Open a plain (non-TLS) TCP connection to `addr` and wrap it in a
    /// [`TransportSession`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the TCP connection cannot be established.
    pub async fn connect_plain(addr: &str) -> Result<Self> {
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| Error::Transport(format!("TCP connect to {addr} failed: {e}")))?;
        tracing::debug!(addr, "plain TCP connection established");
        Ok(Self {
            session: TransportSession::from_stream(tcp),
        })
    }

    /// Send `payload` to the server with the given [`FrameFlags`].
    ///
    /// Delegates directly to [`TransportSession::send_frame`], which assigns
    /// sequence numbers automatically.
    ///
    /// # Errors
    ///
    /// Propagates transport-level errors from the underlying session.
    pub async fn send_payload(&mut self, payload: Bytes, flags: FrameFlags) -> Result<()> {
        self.session.send_frame(payload, flags).await
    }

    /// Receive the next [`Frame`] from the server.
    ///
    /// Blocks until a complete frame arrives. Returns
    /// [`Error::ConnectionClosed`] when the server closes the connection
    /// cleanly.
    ///
    /// # Errors
    ///
    /// Propagates transport-level errors from the underlying session.
    pub async fn recv_frame(&mut self) -> Result<Frame> {
        self.session.recv_frame().await
    }

    /// Gracefully close the session by sending a `SESSION_CLOSE` frame and
    /// flushing the underlying stream.
    ///
    /// # Errors
    ///
    /// Propagates transport-level errors from the underlying session.
    pub async fn close(&mut self) -> Result<()> {
        self.session.close().await
    }

    /// Consume this connection and return the underlying [`TransportSession`].
    ///
    /// Used by the background I/O task to take ownership of the session after
    /// the initial signaling handshake is complete.
    pub fn into_session(self) -> TransportSession {
        self.session
    }
}
