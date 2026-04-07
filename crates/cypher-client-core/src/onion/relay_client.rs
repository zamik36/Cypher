use bytes::Bytes;
use cypher_common::{Error, Result};
use cypher_transport::{FrameFlags, TransportSession};
use std::sync::Arc;

/// Client for relay onion mode.
pub struct RelayClient {
    session: TransportSession,
}

impl RelayClient {
    /// Connect to relay and switch to onion mode by sending `ONION` as the first payload.
    pub async fn connect(addr: &str, tls_config: Arc<rustls::ClientConfig>) -> Result<Self> {
        let mut session = TransportSession::connect(addr, tls_config).await?;
        session
            .send_frame(Bytes::from_static(b"ONION"), FrameFlags::NONE)
            .await?;
        Ok(Self { session })
    }

    /// Send one onion request payload and wait for a response payload.
    pub async fn send_and_recv(&mut self, onion_payload: Vec<u8>) -> Result<Vec<u8>> {
        self.session
            .send_frame(Bytes::from(onion_payload), FrameFlags::NONE)
            .await?;

        loop {
            let frame = self.session.recv_frame().await?;
            if frame.flags.contains(FrameFlags::PING) {
                self.session.send_pong().await?;
                continue;
            }
            if frame.flags.contains(FrameFlags::PONG) {
                continue;
            }
            return Ok(frame.payload.to_vec());
        }
    }

    pub async fn close(&mut self) -> Result<()> {
        self.session
            .close()
            .await
            .map_err(|e| Error::Transport(format!("relay close failed: {e}")))
    }
}
