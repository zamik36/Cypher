//! Gateway, signaling and relay running in this process against real Redis
//! and NATS, on free local ports with fresh development certificates.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::time::Duration;

use cypher_server_kit::NatsConfig;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::Target;

const STARTUP: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

pub struct Stack {
    target: Target,
    /// Metrics (and readiness) endpoints of signaling, gateway and relay.
    metrics: [SocketAddr; 3],
    shutdown: CancellationToken,
    services: Vec<JoinHandle<anyhow::Result<()>>>,
    _dir: TempDir,
}

impl Stack {
    /// Starts the stack when `CYPHER_TEST_REDIS` and `CYPHER_TEST_NATS` name
    /// the backing stores. NATS credentials, if the server enforces the
    /// per-service ACL, come from `{GATEWAY,SIGNALING,RELAY}_NATS_PASSWORD`.
    pub async fn from_env() -> Option<Self> {
        let redis_url = std::env::var("CYPHER_TEST_REDIS").ok()?;
        let nats_url = std::env::var("CYPHER_TEST_NATS").ok()?;
        Some(Self::start(redis_url, &nats_url).await)
    }

    async fn start(redis_url: String, nats_url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (gateway_addr, relay_addr) = (free_local_addr(), free_local_addr());
        let probes = [free_local_addr(), free_local_addr(), free_local_addr()];
        let shutdown = CancellationToken::new();
        let services = vec![
            tokio::spawn(signaling::run(
                signaling::Config {
                    redis_url,
                    nats: nats(nats_url, "signaling"),
                    metrics_addr: probes[0],
                    onion_key_path: dir.path().join("onion.bin"),
                    relay_public_addr: Some(format!("localhost:{}", relay_addr.port())),
                },
                shutdown.clone(),
            )),
            tokio::spawn(gateway::run(
                gateway::Config {
                    gateway_addr,
                    ws_addr: None,
                    nats: nats(nats_url, "gateway"),
                    tls_cert_path: None,
                    tls_key_path: None,
                    dev_cert_out: Some(dir.path().join("gateway.pem")),
                    metrics_addr: probes[1],
                    max_connections: 64,
                    max_connections_per_ip: 64,
                    frames_per_sec: 2_000,
                    bytes_per_sec: 64 << 20,
                },
                shutdown.clone(),
            )),
            tokio::spawn(relay::run(
                relay::Config {
                    relay_addr,
                    ws_addr: None,
                    nats: nats(nats_url, "relay"),
                    tls_cert_path: None,
                    tls_key_path: None,
                    dev_cert_out: Some(dir.path().join("relay.pem")),
                    metrics_addr: probes[2],
                    max_connections: 64,
                    max_connections_per_ip: 64,
                },
                shutdown.clone(),
            )),
        ];
        wait_until_ready(&probes).await;
        let target = Target {
            gateway_addr: format!("localhost:{}", gateway_addr.port()),
            tls: pinned_tls(dir.path()),
        };
        Self {
            target,
            metrics: probes,
            shutdown,
            services,
            _dir: dir,
        }
    }

    pub fn metrics_addrs(&self) -> &[SocketAddr] {
        &self.metrics
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Cancels every service and fails if any of them ended with an error.
    pub async fn stop(self) {
        self.shutdown.cancel();
        for service in self.services {
            service.await.unwrap().unwrap();
        }
    }
}

fn nats(url: &str, user: &str) -> NatsConfig {
    let password = std::env::var(format!("{}_NATS_PASSWORD", user.to_uppercase())).ok();
    NatsConfig {
        url: url.to_owned(),
        user: password.is_some().then(|| user.to_owned()),
        password,
        token: None,
    }
}

fn any_local_port() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

/// A port the OS just handed out; services bind it right after.
fn free_local_addr() -> SocketAddr {
    TcpListener::bind(any_local_port())
        .and_then(|l| l.local_addr())
        .unwrap()
}

/// Every service answers its readiness probe: listeners bound, NATS
/// subscribed. Services write their certificates before binding, so the PEM
/// files are complete.
async fn wait_until_ready(probes: &[SocketAddr]) {
    tokio::time::timeout(STARTUP, async {
        for &addr in probes {
            while tokio::task::spawn_blocking(move || cypher_server_kit::metrics::probe_ready(addr))
                .await
                .unwrap()
                .is_err()
            {
                tokio::time::sleep(POLL).await;
            }
        }
    })
    .await
    .expect("stack not ready in time: are CYPHER_TEST_REDIS and CYPHER_TEST_NATS reachable?");
}

fn pinned_tls(dir: &Path) -> std::sync::Arc<rustls::ClientConfig> {
    let pem = ["gateway.pem", "relay.pem"]
        .map(|name| std::fs::read_to_string(dir.join(name)).unwrap())
        .concat();
    cypher_tls::make_client_config_with_pem(&pem).unwrap()
}
