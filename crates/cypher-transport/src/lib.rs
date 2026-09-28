//! Framed TLS transport: `[u32 BE length][wire frame]`. The payload of each
//! frame is one `cypher-wire` message; decoding hands out zero-copy slices.

use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use cypher_types::{Error, MAX_FRAME_SIZE, Result};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector, client};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub type Conn<S> = Framed<S, LengthDelimitedCodec>;
pub type ClientConn = Conn<client::TlsStream<TcpStream>>;
pub type ServerConn = Conn<tokio_rustls::server::TlsStream<TcpStream>>;

pub fn codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .length_field_type::<u32>()
        .max_frame_length(MAX_FRAME_SIZE)
        .new_codec()
}

/// Read and write buffer of a connection. tokio-util's 8 KiB default made
/// them the largest part of an idle gateway connection; chat frames fit
/// several to a write batch and the buffers grow for file chunks.
const BUFFER: usize = 2048;

pub fn framed<S: AsyncRead + AsyncWrite>(stream: S) -> Conn<S> {
    Framed::with_capacity(stream, codec(), BUFFER)
}

/// Splits `host:port`, accepting bracketed IPv6 literals (`[::1]:443`).
pub fn split_host_port(addr: &str) -> Result<(&str, u16)> {
    let (host, port) = addr
        .rsplit_once(':')
        .ok_or_else(|| Error::Config(format!("missing port in {addr}")))?;
    let port = port
        .parse()
        .map_err(|_| Error::Config(format!("invalid port in {addr}")))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(Error::Config(format!("missing host in {addr}")));
    }
    Ok((host, port))
}

pub async fn connect_tls(addr: &str, tls: Arc<rustls::ClientConfig>) -> Result<ClientConn> {
    let (host, port) = split_host_port(addr)?;
    let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| Error::Timeout)??;
    tcp.set_nodelay(true)?;
    tls_over(tcp, host, tls).await
}

/// TLS client handshake over an already established stream (e.g. a Tor
/// circuit), verifying the certificate for `host`.
pub async fn tls_over<S>(
    stream: S,
    host: &str,
    tls: Arc<rustls::ClientConfig>,
) -> Result<Conn<client::TlsStream<S>>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| Error::Config(format!("invalid server name {host}: {e}")))?;
    let stream = timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(tls).connect(name, stream),
    )
    .await
    .map_err(|_| Error::Timeout)??;
    Ok(framed(stream))
}

/// Server-side TLS handshake with a deadline, so a silent client cannot pin
/// a task (or, if awaited inline, the accept loop) indefinitely.
pub async fn accept_tls(acceptor: &TlsAcceptor, tcp: TcpStream) -> Result<ServerConn> {
    tcp.set_nodelay(true)?;
    let stream = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp))
        .await
        .map_err(|_| Error::Timeout)??;
    Ok(framed(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            split_host_port("example.org:443").unwrap(),
            ("example.org", 443)
        );
        assert_eq!(split_host_port("[::1]:9100").unwrap(), ("::1", 9100));
        split_host_port("nohost").unwrap_err();
        split_host_port(":80").unwrap_err();
        split_host_port("h:99999").unwrap_err();
    }
}

/// Transport-agnostic halves of a frame connection (TLS or WebSocket).
pub type FrameStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send>>;
pub type FrameSink =
    std::pin::Pin<Box<dyn futures::Sink<bytes::Bytes, Error = std::io::Error> + Send>>;

/// Splits a framed connection into boxed halves.
pub fn split<S>(conn: Conn<S>) -> (FrameStream, FrameSink)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    use futures::StreamExt;
    let (sink, stream) = conn.split();
    (
        Box::pin(stream.map(|r| r.map(BytesMut::freeze))),
        Box::pin(sink),
    )
}

/// Accepts the next TCP connection; accept errors (e.g. fd exhaustion) are
/// transient, so this backs off instead of failing the listener.
pub async fn accept(listener: &tokio::net::TcpListener) -> TcpStream {
    loop {
        match listener.accept().await {
            Ok((tcp, _)) => return tcp,
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

pub mod server;
#[cfg(feature = "ws")]
pub mod ws;
