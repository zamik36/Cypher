//! Gateway: terminates client connections (TLS and WebSocket), authenticates
//! identities and relays opaque end-to-end frames between peers.

mod bus;
mod metrics;
mod outbox;
mod registry;
mod session;
#[cfg(test)]
mod tests;

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_types::MAX_FRAME_SIZE;
use futures::{SinkExt, StreamExt, future};
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::{info, warn};

use session::{Gateway, Limits};

type NatsGateway = Gateway<async_nats::Client>;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_gateway_addr")]
    gateway_addr: SocketAddr,
    ws_addr: Option<SocketAddr>,
    #[serde(flatten)]
    nats: cypher_server_kit::NatsConfig,
    tls_cert_path: Option<String>,
    tls_key_path: Option<String>,
    /// Where to write the development certificate for local clients to pin.
    dev_cert_out: Option<PathBuf>,
    #[serde(default = "default_metrics_addr")]
    metrics_addr: SocketAddr,
    #[serde(default = "default_max_connections")]
    max_connections: usize,
    #[serde(default = "default_frames_per_sec")]
    frames_per_sec: u64,
    #[serde(default = "default_bytes_per_sec")]
    bytes_per_sec: u64,
}

fn default_gateway_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9100))
}
fn default_metrics_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9090))
}
fn default_max_connections() -> usize {
    100_000
}
fn default_frames_per_sec() -> u64 {
    2_000
}
fn default_bytes_per_sec() -> u64 {
    64 << 20
}

const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_server_kit::init_tracing();
    let config: Config = cypher_server_kit::load_config()?;
    cypher_server_kit::metrics::spawn_metrics_server(config.metrics_addr);

    let tls = cypher_tls::load_server_config(
        config.tls_cert_path.as_deref(),
        config.tls_key_path.as_deref(),
        &["localhost", "127.0.0.1"],
        config.dev_cert_out.as_deref(),
    )
    .await?;
    let nats = cypher_server_kit::connect_nats(&config.nats).await?;
    let gateway = Arc::new(Gateway::new(
        nats,
        Limits {
            frames_per_sec: config.frames_per_sec,
            bytes_per_sec: config.bytes_per_sec,
        },
        metrics::Metrics::register(),
    ));
    let permits = Arc::new(Semaphore::new(config.max_connections));

    let tls_listener = TcpListener::bind(config.gateway_addr).await?;
    info!(addr = %config.gateway_addr, "gateway TLS listening");
    let tls_loop = accept_tls(
        tls_listener,
        TlsAcceptor::from(tls),
        Arc::clone(&gateway),
        Arc::clone(&permits),
    );
    let ws_loop = async {
        match config.ws_addr {
            Some(addr) => {
                let listener = TcpListener::bind(addr).await?;
                info!(%addr, "gateway WebSocket listening");
                accept_ws(listener, Arc::clone(&gateway), Arc::clone(&permits)).await
            }
            None => future::pending().await,
        }
    };

    tokio::select! {
        r = tls_loop => r?,
        r = ws_loop => r?,
        () = cypher_server_kit::shutdown_signal() => info!("shutdown signal received"),
    }
    Ok(())
}

async fn accept_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    gateway: Arc<NatsGateway>,
    permits: Arc<Semaphore>,
) -> anyhow::Result<()> {
    loop {
        let (tcp, _) = accept(&listener).await;
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            gateway.metrics.rejected.inc();
            continue;
        };
        let acceptor = acceptor.clone();
        let gateway = Arc::clone(&gateway);
        tokio::spawn(async move {
            if let Ok(conn) = cypher_transport::accept_tls(&acceptor, tcp).await {
                let (sink, stream) = conn.split();
                let stream = stream.map(|r| r.map(bytes::BytesMut::freeze));
                gateway.serve(stream, sink).await;
            }
            drop(permit);
        });
    }
}

async fn accept_ws(
    listener: TcpListener,
    gateway: Arc<NatsGateway>,
    permits: Arc<Semaphore>,
) -> anyhow::Result<()> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_SIZE))
        .max_frame_size(Some(MAX_FRAME_SIZE));
    loop {
        let (tcp, _) = accept(&listener).await;
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            gateway.metrics.rejected.inc();
            continue;
        };
        let gateway = Arc::clone(&gateway);
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let handshake = tokio_tungstenite::accept_async_with_config(tcp, Some(config));
            if let Ok(Ok(ws)) = tokio::time::timeout(WS_HANDSHAKE_TIMEOUT, handshake).await {
                let (sink, stream) = ws.split();
                let stream = stream.filter_map(|m| {
                    future::ready(match m {
                        Ok(Message::Binary(b)) => Some(Ok(b)),
                        Ok(Message::Close(_)) | Err(_) => {
                            Some(Err(io::ErrorKind::ConnectionAborted.into()))
                        }
                        Ok(_) => None,
                    })
                });
                let sink = sink
                    .sink_map_err(|e| io::Error::other(e.to_string()))
                    .with(|b: Bytes| future::ready(Ok::<_, io::Error>(Message::Binary(b))));
                gateway.serve(Box::pin(stream), Box::pin(sink)).await;
            }
            drop(permit);
        });
    }
}

/// Accept errors (e.g. fd exhaustion) are transient: back off, never exit.
async fn accept(listener: &TcpListener) -> (TcpStream, SocketAddr) {
    loop {
        match listener.accept().await {
            Ok(pair) => return pair,
            Err(e) => {
                warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
