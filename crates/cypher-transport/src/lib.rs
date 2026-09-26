//! Framed TLS transport: `[u32 BE length][wire frame]`. The payload of each
//! frame is one `cypher-wire` message; decoding hands out zero-copy slices.

use std::sync::Arc;
use std::time::Duration;

use cypher_types::{Error, MAX_FRAME_SIZE, Result};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector, client, server};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub type Conn<S> = Framed<S, LengthDelimitedCodec>;
pub type ClientConn = Conn<client::TlsStream<TcpStream>>;
pub type ServerConn = Conn<server::TlsStream<TcpStream>>;

pub fn codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .length_field_type::<u32>()
        .max_frame_length(MAX_FRAME_SIZE)
        .new_codec()
}

pub fn framed<S: AsyncRead + AsyncWrite>(stream: S) -> Conn<S> {
    Framed::new(stream, codec())
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
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| Error::Config(format!("invalid server name {host}: {e}")))?;
    let stream = timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(tls).connect(name, tcp),
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
        assert!(split_host_port("nohost").is_err());
        assert!(split_host_port(":80").is_err());
        assert!(split_host_port("h:99999").is_err());
    }
}
