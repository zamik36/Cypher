//! Gateway: terminates client connections (TLS and WebSocket), authenticates
//! identities and relays opaque end-to-end frames between peers.

mod bus;
mod metrics;
mod outbox;
mod registry;
mod session;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::ensure;
use cypher_server_kit::ratelimit::BURST_SECS;
use cypher_transport::server::{self, Handler, Listener, Upgrade};
use cypher_transport::{FrameSink, FrameStream};
use cypher_types::MAX_FRAME_SIZE;
use serde::Deserialize;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tracing::info;

pub use bus::Bus;
pub use session::{Gateway, Limits};

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_gateway_addr")]
    pub gateway_addr: SocketAddr,
    pub ws_addr: Option<SocketAddr>,
    #[serde(flatten)]
    pub nats: cypher_server_kit::NatsConfig,
    pub tls_cert_path: Option<String>,
    pub tls_key_path: Option<String>,
    /// Where to write the development certificate for local clients to pin.
    pub dev_cert_out: Option<PathBuf>,
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: SocketAddr,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Connections one client address (IPv4, or IPv6 /64) may hold.
    #[serde(default = "default_max_connections_per_ip")]
    pub max_connections_per_ip: usize,
    #[serde(default = "default_frames_per_sec")]
    pub frames_per_sec: u64,
    #[serde(default = "default_bytes_per_sec")]
    pub bytes_per_sec: u64,
}

fn default_gateway_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9100))
}
fn default_metrics_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9090))
}
impl Config {
    /// Refuses settings under which the gateway would start yet admit no
    /// one, or never pass a full-size frame.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.max_connections > 0 && self.max_connections_per_ip > 0,
            "max_connections and max_connections_per_ip must be positive"
        );
        ensure!(self.frames_per_sec > 0, "frames_per_sec must be positive");
        ensure!(
            self.bytes_per_sec.saturating_mul(BURST_SECS) >= MAX_FRAME_SIZE as u64,
            "bytes_per_sec must let a {MAX_FRAME_SIZE}-byte frame through within {BURST_SECS} s"
        );
        Ok(())
    }
}

fn default_max_connections_per_ip() -> usize {
    128
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

impl<B: Bus> Handler for Gateway<B> {
    fn serve(
        self: Arc<Self>,
        stream: FrameStream,
        sink: FrameSink,
    ) -> impl Future<Output = ()> + Send {
        self.handle(stream, sink)
    }

    fn rejected(&self) {
        self.metrics.rejected.inc();
    }
}

/// Runs the gateway until `shutdown` is cancelled.
pub async fn run(config: Config, shutdown: CancellationToken) -> anyhow::Result<()> {
    config.validate()?;
    let registry = cypher_server_kit::metrics::Metrics::new()?;
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
    let gateway = Arc::new(Gateway::new(
        nats.clone(),
        Limits {
            frames_per_sec: config.frames_per_sec,
            bytes_per_sec: config.bytes_per_sec,
        },
        metrics::Metrics::register(&registry)?,
    ));

    let mut listeners =
        vec![Listener::bind(config.gateway_addr, Upgrade::Tls(TlsAcceptor::from(tls))).await?];
    info!(addr = %config.gateway_addr, "gateway TLS listening");
    if let Some(addr) = config.ws_addr {
        listeners.push(Listener::bind(addr, Upgrade::WebSocket).await?);
        info!(%addr, "gateway WebSocket listening");
    }
    let limits = cypher_transport::ConnectionLimits {
        total: config.max_connections,
        per_ip: config.max_connections_per_ip,
    };
    registry.set_ready(nats);
    server::serve(listeners, gateway, limits, shutdown).await;
    Ok(())
}
