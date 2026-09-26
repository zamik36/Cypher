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

use futures::future;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tracing::info;

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

/// How accepted TCP connections are upgraded before serving.
#[derive(Clone)]
enum Upgrade {
    Tls(TlsAcceptor),
    WebSocket,
}

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
    let tls_loop = accept_loop(
        tls_listener,
        Upgrade::Tls(TlsAcceptor::from(tls)),
        Arc::clone(&gateway),
        Arc::clone(&permits),
    );
    let ws_loop = async {
        match config.ws_addr {
            Some(addr) => {
                let listener = TcpListener::bind(addr).await?;
                info!(%addr, "gateway WebSocket listening");
                accept_loop(
                    listener,
                    Upgrade::WebSocket,
                    Arc::clone(&gateway),
                    Arc::clone(&permits),
                )
                .await;
                Ok(())
            }
            None => future::pending::<anyhow::Result<()>>().await,
        }
    };

    tokio::select! {
        () = tls_loop => {}
        r = ws_loop => r?,
        () = cypher_server_kit::shutdown_signal() => info!("shutdown signal received"),
    }
    Ok(())
}

async fn accept_loop(
    listener: TcpListener,
    upgrade: Upgrade,
    gateway: Arc<NatsGateway>,
    permits: Arc<Semaphore>,
) {
    loop {
        let tcp = cypher_transport::accept(&listener).await;
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            gateway.metrics.rejected.inc();
            continue;
        };
        let (upgrade, gateway) = (upgrade.clone(), Arc::clone(&gateway));
        tokio::spawn(async move {
            let halves = match upgrade {
                Upgrade::Tls(acceptor) => cypher_transport::accept_tls(&acceptor, tcp)
                    .await
                    .map(cypher_transport::split),
                Upgrade::WebSocket => cypher_transport::ws::accept_ws(tcp).await,
            };
            if let Ok((stream, sink)) = halves {
                gateway.serve(stream, sink).await;
            }
            drop(permit);
        });
    }
}
