//! Onion relay: forwards sealed anonymous requests to signaling. It learns
//! client addresses but can neither read nor forge requests or replies, and
//! keeps no state beyond open connections.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use cypher_server_kit::ratelimit::ConnLimiter;
use cypher_server_kit::{SIG_ONION_SUBJECT, metrics};
use futures::stream::FuturesUnordered;
use futures::{SinkExt, StreamExt};
use prometheus::{IntCounter, IntGauge};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

const CORR_LEN: usize = 8;
const MAX_REQUEST: usize = 128 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const PIPELINE: usize = 8;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_relay_addr")]
    relay_addr: SocketAddr,
    #[serde(flatten)]
    nats: cypher_server_kit::NatsConfig,
    tls_cert_path: Option<String>,
    tls_key_path: Option<String>,
    dev_cert_out: Option<PathBuf>,
    #[serde(default = "default_metrics_addr")]
    metrics_addr: SocketAddr,
    #[serde(default = "default_max_connections")]
    max_connections: usize,
}

fn default_relay_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9300))
}
fn default_metrics_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9092))
}
fn default_max_connections() -> usize {
    20_000
}

struct Relay {
    nats: async_nats::Client,
    connections: IntGauge,
    forwarded: IntCounter,
    dropped: IntCounter,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_server_kit::init_tracing();
    let config: Config = cypher_server_kit::load_config()?;
    metrics::spawn_metrics_server(config.metrics_addr);

    let tls = cypher_tls::load_server_config(
        config.tls_cert_path.as_deref(),
        config.tls_key_path.as_deref(),
        &["localhost", "127.0.0.1"],
        config.dev_cert_out.as_deref(),
    )
    .await?;
    let relay = Arc::new(Relay {
        nats: cypher_server_kit::connect_nats(&config.nats).await?,
        connections: metrics::gauge("relay_connections", "Open onion connections"),
        forwarded: metrics::counter("relay_forwarded_total", "Onion requests forwarded"),
        dropped: metrics::counter("relay_dropped_total", "Onion requests dropped"),
    });
    let listener = TcpListener::bind(config.relay_addr).await?;
    info!(addr = %config.relay_addr, "relay listening");

    tokio::select! {
        () = accept_loop(listener, TlsAcceptor::from(tls), relay, config.max_connections) => {}
        () = cypher_server_kit::shutdown_signal() => info!("shutdown signal received"),
    }
    Ok(())
}

async fn accept_loop(listener: TcpListener, acceptor: TlsAcceptor, relay: Arc<Relay>, max: usize) {
    let permits = Arc::new(Semaphore::new(max));
    loop {
        let tcp = match listener.accept().await {
            Ok((tcp, _)) => tcp,
            Err(e) => {
                warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            continue;
        };
        let (acceptor, relay) = (acceptor.clone(), Arc::clone(&relay));
        tokio::spawn(async move {
            if let Ok(conn) = cypher_transport::accept_tls(&acceptor, tcp).await {
                relay.connections.inc();
                relay.serve(conn).await;
                relay.connections.dec();
            }
            drop(permit);
        });
    }
}

impl Relay {
    async fn serve(&self, conn: cypher_transport::ServerConn) {
        let (mut sink, mut stream) = conn.split();
        let mut limiter = ConnLimiter::new(20, 1 << 20);
        let mut in_flight = FuturesUnordered::new();
        loop {
            tokio::select! {
                frame = tokio::time::timeout(IDLE_TIMEOUT, stream.next()), if in_flight.len() < PIPELINE => {
                    let Ok(Some(Ok(frame))) = frame else { break };
                    if frame.len() <= CORR_LEN || frame.len() > MAX_REQUEST || !limiter.admit(frame.len()) {
                        self.dropped.inc();
                        continue;
                    }
                    in_flight.push(self.forward(frame.freeze()));
                }
                Some(reply) = in_flight.next(), if !in_flight.is_empty() => {
                    if let Some(reply) = reply
                        && sink.send(reply).await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    }

    async fn forward(&self, frame: Bytes) -> Option<Bytes> {
        let corr = &frame[..CORR_LEN];
        let request = self
            .nats
            .request(SIG_ONION_SUBJECT, frame.slice(CORR_LEN..));
        let reply = match tokio::time::timeout(REQUEST_TIMEOUT, request).await {
            Ok(Ok(msg)) => msg.payload,
            _ => {
                self.dropped.inc();
                return None;
            }
        };
        self.forwarded.inc();
        let mut out = BytesMut::with_capacity(CORR_LEN + reply.len());
        out.put_slice(corr);
        out.put_slice(&reply);
        Some(out.freeze())
    }
}
