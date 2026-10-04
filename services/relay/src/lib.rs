//! Onion relay: forwards sealed anonymous requests to signaling. It learns
//! client addresses but can neither read nor forge requests or replies, and
//! keeps no state beyond open connections.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::ensure;
use bytes::{BufMut, Bytes, BytesMut};
use cypher_server_kit::SIG_ONION_SUBJECT;
use cypher_server_kit::metrics::Metrics;
use cypher_server_kit::ratelimit::ConnLimiter;
use cypher_transport::server::{self, Handler, Listener, Upgrade};
use cypher_transport::{FrameSink, FrameStream};
use futures::stream::FuturesUnordered;
use futures::{SinkExt, StreamExt};
use prometheus::{IntCounter, IntGauge};
use serde::Deserialize;
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Client-chosen correlation id echoed in front of each reply.
const CORR_LEN: usize = 8;
const MAX_REQUEST: usize = 128 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Requests one connection may have in flight at once.
const PIPELINE: usize = 8;
const FRAMES_PER_SEC: u64 = 20;
const BYTES_PER_SEC: u64 = 1 << 20;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_relay_addr")]
    pub relay_addr: SocketAddr,
    /// WebSocket listener for browsers (put behind a TLS proxy).
    pub ws_addr: Option<SocketAddr>,
    #[serde(flatten)]
    pub nats: cypher_server_kit::NatsConfig,
    pub tls_cert_path: Option<String>,
    pub tls_key_path: Option<String>,
    pub dev_cert_out: Option<PathBuf>,
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: SocketAddr,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Connections one client address (IPv4, or IPv6 /64) may hold.
    #[serde(default = "default_max_connections_per_ip")]
    pub max_connections_per_ip: usize,
}

impl Config {
    /// Refuses settings under which the relay would start yet admit no one.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.max_connections > 0 && self.max_connections_per_ip > 0,
            "max_connections and max_connections_per_ip must be positive"
        );
        Ok(())
    }
}

fn default_relay_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9300))
}
fn default_metrics_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9092))
}
fn default_max_connections_per_ip() -> usize {
    128
}
fn default_max_connections() -> usize {
    20_000
}

/// Where sealed onion requests are answered: signaling over NATS in
/// production. `None` means the request was lost and is dropped silently.
pub trait OnionUpstream: Send + Sync + 'static {
    fn forward(&self, request: Bytes) -> impl Future<Output = Option<Bytes>> + Send;
}

impl OnionUpstream for async_nats::Client {
    async fn forward(&self, request: Bytes) -> Option<Bytes> {
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, self.request(SIG_ONION_SUBJECT, request));
        Some(reply.await.ok()?.ok()?.payload)
    }
}

pub struct RelayMetrics {
    connections: IntGauge,
    forwarded: IntCounter,
    dropped: IntCounter,
    rejected: IntCounter,
}

impl RelayMetrics {
    pub fn register(r: &Metrics) -> anyhow::Result<Self> {
        Ok(Self {
            connections: r.gauge("relay_connections", "Open onion connections")?,
            forwarded: r.counter("relay_forwarded_total", "Onion requests forwarded")?,
            dropped: r.counter("relay_dropped_total", "Onion requests dropped")?,
            rejected: r.counter("relay_rejected_total", "Connections rejected at capacity")?,
        })
    }
}

pub struct Relay<U> {
    upstream: U,
    metrics: RelayMetrics,
}

impl<U: OnionUpstream> Relay<U> {
    pub fn new(upstream: U, metrics: RelayMetrics) -> Self {
        Self { upstream, metrics }
    }

    async fn relay(&self, mut stream: FrameStream, mut sink: FrameSink) {
        let mut limiter = ConnLimiter::new(FRAMES_PER_SEC, BYTES_PER_SEC);
        let mut in_flight = FuturesUnordered::new();
        // Re-armed only when it fires, not per frame (see the gateway session).
        let idle = tokio::time::sleep(IDLE_TIMEOUT);
        tokio::pin!(idle);
        let mut last_frame = Instant::now();
        loop {
            tokio::select! {
                () = &mut idle => {
                    let deadline = last_frame + IDLE_TIMEOUT;
                    if deadline <= Instant::now() {
                        break;
                    }
                    idle.as_mut().reset(deadline);
                }
                frame = stream.next(), if in_flight.len() < PIPELINE => {
                    let Some(Ok(frame)) = frame else { break };
                    last_frame = Instant::now();
                    if frame.len() <= CORR_LEN || frame.len() > MAX_REQUEST || !limiter.admit(frame.len()) {
                        self.metrics.dropped.inc();
                        continue;
                    }
                    in_flight.push(self.forward(frame));
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

    /// `frame` is `[corr u64][sealed request]`; the reply keeps the same
    /// correlation prefix so the client can match pipelined answers.
    async fn forward(&self, frame: Bytes) -> Option<Bytes> {
        let (corr, request) = (frame.slice(..CORR_LEN), frame.slice(CORR_LEN..));
        let Some(reply) = self.upstream.forward(request).await else {
            self.metrics.dropped.inc();
            return None;
        };
        self.metrics.forwarded.inc();
        let mut out = BytesMut::with_capacity(CORR_LEN + reply.len());
        out.put_slice(&corr);
        out.put_slice(&reply);
        Some(out.freeze())
    }
}

impl<U: OnionUpstream> Handler for Relay<U> {
    async fn serve(self: Arc<Self>, stream: FrameStream, sink: FrameSink) {
        self.metrics.connections.inc();
        self.relay(stream, sink).await;
        self.metrics.connections.dec();
    }

    fn rejected(&self) {
        self.metrics.rejected.inc();
    }
}

/// Runs the relay until `shutdown` is cancelled.
pub async fn run(config: Config, shutdown: CancellationToken) -> anyhow::Result<()> {
    config.validate()?;
    let registry = Metrics::new()?;
    registry
        .serve(config.metrics_addr, shutdown.clone())
        .await?;
    let tls = cypher_tls::load_server_config(
        config.tls_cert_path.as_deref(),
        config.tls_key_path.as_deref(),
        &["localhost", "127.0.0.1"],
        config.dev_cert_out.as_deref(),
    )
    .await?;
    let nats = cypher_server_kit::connect_nats(&config.nats).await?;
    let relay = Arc::new(Relay::new(nats.clone(), RelayMetrics::register(&registry)?));

    let mut listeners =
        vec![Listener::bind(config.relay_addr, Upgrade::Tls(TlsAcceptor::from(tls))).await?];
    info!(addr = %config.relay_addr, "relay TLS listening");
    if let Some(addr) = config.ws_addr {
        listeners.push(Listener::bind(addr, Upgrade::WebSocket).await?);
        info!(%addr, "relay WebSocket listening");
    }
    let limits = cypher_transport::ConnectionLimits {
        total: config.max_connections,
        per_ip: config.max_connections_per_ip,
    };
    registry.set_ready(nats);
    server::serve(listeners, relay, limits, shutdown).await;
    Ok(())
}

#[cfg(test)]
mod tests;
