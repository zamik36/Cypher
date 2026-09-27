//! Reaches the onion relay through Tor, so neither the relay nor signaling
//! learns the client's network address. The Tor client bootstraps lazily
//! and is shared by all reconnects.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arti_client::config::pt::TransportConfigBuilder;
use arti_client::config::{BridgeConfigBuilder, CfgPath, TorClientConfigBuilder};
use arti_client::{TorClient, TorClientConfig};
use tokio::sync::{OnceCell, mpsc};
use tokio::time::timeout;
use tor_rtcompat::PreferredRuntime;

use crate::TorConfig;
use crate::net::{self, Link, NetEvent};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Censored networks can stall bootstrap forever; give up and let the
/// driver retry (or the user add bridges) instead of hanging the channel.
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(90);
const CIRCUIT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct Tor {
    config: TorConfig,
    dir: PathBuf,
    client: OnceCell<Arc<TorClient<PreferredRuntime>>>,
}

impl Tor {
    pub(crate) fn new(config: TorConfig, dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            config,
            dir,
            client: OnceCell::new(),
        })
    }

    async fn client(&self) -> Result<&Arc<TorClient<PreferredRuntime>>, BoxError> {
        self.client
            .get_or_try_init(|| async {
                let _ = rustls::crypto::ring::default_provider().install_default();
                let config = build_config(&self.config, &self.dir)?;
                let client = timeout(BOOTSTRAP_TIMEOUT, TorClient::create_bootstrapped(config))
                    .await
                    .map_err(|_| "tor bootstrap timed out")??;
                Ok::<_, BoxError>(client)
            })
            .await
    }

    /// Connects to `addr` over a Tor circuit, then runs TLS inside it.
    pub(crate) fn spawn_relay(
        self: &Arc<Self>,
        addr: String,
        tls: Arc<rustls::ClientConfig>,
        events: mpsc::UnboundedSender<NetEvent>,
    ) {
        let tor = Arc::clone(self);
        tokio::spawn(async move {
            let conn = async {
                let (host, port) = cypher_transport::split_host_port(&addr)?;
                let client = tor.client().await?;
                let stream = timeout(CIRCUIT_TIMEOUT, client.connect((host, port)))
                    .await
                    .map_err(|_| "tor circuit timed out")??;
                Ok::<_, BoxError>(cypher_transport::tls_over(stream, host, tls).await?)
            };
            match conn.await {
                Ok(conn) => net::pump(Link::Relay, conn, events).await,
                Err(e) => {
                    tracing::warn!("tor relay connection failed: {e}");
                    let _ = events.send(NetEvent::Down(Link::Relay));
                }
            }
        });
    }
}

fn build_config(config: &TorConfig, dir: &std::path::Path) -> Result<TorClientConfig, BoxError> {
    let mut builder =
        TorClientConfigBuilder::from_directories(dir.join("state"), dir.join("cache"));
    for line in &config.bridges {
        let bridge: BridgeConfigBuilder = line.parse()?;
        builder.bridges().bridges().push(bridge);
    }
    if let Some(binary) = &config.transport_binary {
        let mut transport = TransportConfigBuilder::default();
        transport
            .protocols(vec!["obfs4".parse()?, "webtunnel".parse()?])
            .path(CfgPath::new_literal(binary))
            .run_on_startup(true);
        builder.bridges().transports().push(transport);
    }
    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_lines_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let ok = TorConfig {
            bridges: vec![
                "192.0.2.1:443 0123456789ABCDEF0123456789ABCDEF01234567".into(),
                "obfs4 192.0.2.2:443 0123456789ABCDEF0123456789ABCDEF01234567 cert=AAAA iat-mode=0"
                    .into(),
            ],
            transport_binary: Some("lyrebird".into()),
        };
        build_config(&ok, dir.path()).unwrap();
        let bad = TorConfig {
            bridges: vec!["definitely not a bridge".into()],
            transport_binary: None,
        };
        build_config(&bad, dir.path()).unwrap_err();
    }

    /// Needs Internet access: `CYPHER_TOR_SMOKE=1 cargo test -p cypher-client --features tor`.
    #[tokio::test(flavor = "multi_thread")]
    async fn tls_through_a_tor_circuit() {
        if std::env::var("CYPHER_TOR_SMOKE").is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let tor = Tor::new(TorConfig::default(), dir.path().to_owned());
        let client = match tor.client().await {
            Ok(client) => client,
            Err(e) => panic!("tor bootstrap failed (network may block Tor; use bridges): {e}"),
        };
        let stream = client.connect(("example.com", 443)).await.unwrap();
        let tls = cypher_transport::tls_over(stream, "example.com", cypher_tls_roots()).await;
        assert!(tls.is_ok(), "TLS over Tor failed: {:?}", tls.err());
    }

    fn cypher_tls_roots() -> Arc<rustls::ClientConfig> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }
}
