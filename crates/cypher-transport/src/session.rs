use std::sync::Arc;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::codec::Framed;
use tracing::{debug, instrument};

use cypher_common::{Error, Result};

use crate::codec::FrameCodec;
use crate::frame::{Frame, FrameFlags};

/// A framed transport session over a TLS-secured TCP connection.
///
/// Tracks sequence numbers and provides helpers for control frames
/// (PING, PONG, SESSION_CLOSE).
pub struct TransportSession {
    inner: Framed<Box<dyn AsyncReadWrite>, FrameCodec>,
    send_seq: u32,
    recv_ack: u32,
}

/// Helper trait alias so we can box the TLS stream.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

impl TransportSession {
    pub fn from_stream(stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static) -> Self {
        Self {
            inner: Framed::new(
                Box::new(stream) as Box<dyn AsyncReadWrite>,
                FrameCodec::new(),
            ),
            send_seq: 0,
            recv_ack: 0,
        }
    }

    #[instrument(skip(tls_config))]
    pub async fn connect(addr: &str, tls_config: Arc<rustls::ClientConfig>) -> Result<Self> {
        let addr = addr
            .strip_prefix("wss://")
            .or_else(|| addr.strip_prefix("ws://"))
            .unwrap_or(addr);
        let tcp = TcpStream::connect(addr).await?;
        let connector = TlsConnector::from(tls_config);

        // Derive a ServerName from the address (strip port).
        let host = addr.split(':').next().unwrap_or(addr);

        let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|e| Error::Transport(format!("invalid server name: {e}")))?;

        let tls_stream = connector.connect(server_name, tcp).await?;

        debug!("TLS connection established to {addr}");

        Ok(Self::from_stream(tls_stream))
    }

    /// Send a frame with the given payload and flags.
    ///
    /// Automatically assigns the next sequence number and the latest
    /// acknowledged receive sequence.
    pub async fn send_frame(&mut self, payload: Bytes, flags: FrameFlags) -> Result<()> {
        self.send_seq = self.send_seq.wrapping_add(1);
        let frame = Frame::new(self.send_seq, self.recv_ack, flags, payload);
        self.inner
            .send(frame)
            .await
            .map_err(|e| Error::Transport(format!("send error: {e}")))?;
        Ok(())
    }

    /// Receive the next frame from the peer.
    ///
    /// Updates the internal ack counter with the received frame's sequence
    /// number.
    pub async fn recv_frame(&mut self) -> Result<Frame> {
        let frame = self
            .inner
            .next()
            .await
            .ok_or(Error::ConnectionClosed)?
            .map_err(|e| Error::Transport(format!("recv error: {e}")))?;

        self.recv_ack = frame.seq_no;
        Ok(frame)
    }

    pub async fn send_ping(&mut self) -> Result<()> {
        debug!("sending PING");
        self.send_frame(Bytes::new(), FrameFlags::PING).await
    }

    pub async fn send_pong(&mut self) -> Result<()> {
        debug!("sending PONG");
        self.send_frame(Bytes::new(), FrameFlags::PONG).await
    }

    /// Gracefully close the session by sending a SESSION_CLOSE frame.
    pub async fn close(&mut self) -> Result<()> {
        debug!("sending SESSION_CLOSE");
        self.send_frame(Bytes::new(), FrameFlags::SESSION_CLOSE)
            .await?;
        self.inner
            .close()
            .await
            .map_err(|e| Error::Transport(format!("close error: {e}")))?;
        Ok(())
    }
}
