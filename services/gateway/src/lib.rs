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

use cypher_transport::server::{self, Handler, Listener, Upgrade};
use cypher_transport::{FrameSink, FrameStream};
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
        nats,
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
    server::serve(listeners, gateway, config.max_connections, shutdown).await;
    Ok(())
}
