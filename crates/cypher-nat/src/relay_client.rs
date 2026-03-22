//! TCP/TLS client for connecting to the Relay service.
//!
//! When P2P hole punching fails (e.g. both peers behind Symmetric NAT),
//! the client falls back to the relay. The relay never sees plaintext —
//! all data is E2EE by the Double Ratchet layer above.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;
use tracing::{debug, info};

use cypher_common::{Error, Result};
use cypher_transport::codec::FrameCodec;
use cypher_transport::frame::{Frame, FrameFlags};
use cypher_transport::session::AsyncReadWrite;

/// A client connected to the relay service over TLS.
///
/// After [`connect`](Self::connect), the first frame sent is the session key.
/// Subsequent frames are forwarded by the relay to the paired peer.
pub struct RelayClient {
    framed: Framed<Box<dyn AsyncReadWrite>, FrameCodec>,
}

impl RelayClient {
    /// Connect to a relay server at `addr` (host:port) over TLS and register
    /// with the given `session_key`.
    ///
    /// The relay pairs two peers that present the same session key.
    pub async fn connect(addr: &str, session_key: &str) -> Result<Self> {
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| Error::Transport(format!("relay TCP connect to {addr}: {e}")))?;

        // TLS handshake.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls_config = cypher_tls::make_client_config();
        let connector = tokio_rustls::TlsConnector::from(tls_config);

        let host = addr.split(':').next().unwrap_or("localhost");
        let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|e| Error::Transport(format!("invalid relay hostname: {e}")))?;

        let tls_stream = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| Error::Transport(format!("relay TLS handshake: {e}")))?;

        info!(relay = addr, "TLS connection to relay established");

        Self::from_stream(Box::new(tls_stream), session_key).await
    }

    /// Connect to a relay server **without** TLS (for development / testing).
    pub async fn connect_plain(addr: &str, session_key: &str) -> Result<Self> {
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| Error::Transport(format!("relay TCP connect to {addr}: {e}")))?;

        Self::from_stream(Box::new(tcp), session_key).await
    }

    async fn from_stream(stream: Box<dyn AsyncReadWrite>, session_key: &str) -> Result<Self> {
        let mut framed = Framed::new(stream, FrameCodec::new());

        let key_frame = Frame::new(0, 0, FrameFlags::NONE, Bytes::from(session_key.to_string()));
        framed
            .send(key_frame)
            .await
            .map_err(|e| Error::Transport(format!("relay send session key: {e}")))?;

        debug!(session_key = session_key, "relay session key sent");

        Ok(Self { framed })
    }

    pub async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.framed
            .send(frame)
            .await
            .map_err(|e| Error::Transport(format!("relay send: {e}")))
    }

    /// Receive the next frame from the relay.
    ///
    /// Returns `None` if the connection was closed.
    pub async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        match self.framed.next().await {
            Some(Ok(frame)) => Ok(Some(frame)),
            Some(Err(e)) => Err(Error::Transport(format!("relay recv: {e}"))),
            None => Ok(None),
        }
    }

    /// Split into (sender, receiver) halves for concurrent use.
    #[allow(clippy::type_complexity)]
    pub fn split(
        self,
    ) -> (
        futures::stream::SplitSink<Framed<Box<dyn AsyncReadWrite>, FrameCodec>, Frame>,
        futures::stream::SplitStream<Framed<Box<dyn AsyncReadWrite>, FrameCodec>>,
    ) {
        self.framed.split()
    }
}
