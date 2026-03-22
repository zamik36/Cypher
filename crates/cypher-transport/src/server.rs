use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, instrument};

use cypher_common::{Error, Result};

use crate::session::TransportSession;

pub struct TransportListener {
    tcp_listener: TcpListener,
    tls_acceptor: TlsAcceptor,
}

impl TransportListener {
    #[instrument(skip(tls_config))]
    pub async fn bind(addr: &str, tls_config: Arc<rustls::ServerConfig>) -> Result<Self> {
        let tcp_listener = TcpListener::bind(addr).await?;
        let tls_acceptor = TlsAcceptor::from(tls_config);

        info!("transport listener bound to {addr}");

        Ok(Self {
            tcp_listener,
            tls_acceptor,
        })
    }

    pub async fn accept(&mut self) -> Result<TransportSession> {
        let (tcp_stream, peer_addr) = self.tcp_listener.accept().await?;
        debug!("accepted TCP connection from {peer_addr}");

        let tls_stream = self
            .tls_acceptor
            .accept(tcp_stream)
            .await
            .map_err(|e| Error::Transport(format!("TLS accept error: {e}")))?;

        debug!("TLS handshake complete with {peer_addr}");

        Ok(TransportSession::from_stream(tls_stream))
    }
}
