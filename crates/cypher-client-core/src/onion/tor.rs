#[cfg(feature = "tor")]
use std::collections::HashSet;
#[cfg(feature = "tor")]
use std::sync::Arc;

#[cfg(feature = "tor")]
use arti_client::{
    config::{BoolOrAuto, BridgeConfigBuilder, TorClientConfigBuilder},
    TorClient, TorClientConfig,
};
#[cfg(feature = "tor")]
use cypher_common::{Error, Result};
#[cfg(feature = "tor")]
use cypher_transport::TransportSession;
#[cfg(feature = "tor")]
use rand::seq::SliceRandom;
#[cfg(feature = "tor")]
use tokio_rustls::TlsConnector;
#[cfg(feature = "tor")]
use tor_rtcompat::PreferredRuntime;

#[cfg(feature = "tor")]
use crate::connection::ServerConnection;
#[cfg(feature = "tor")]
use crate::onion::config::TorSettings;
#[cfg(feature = "tor")]
use crate::onion::shadow::ShadowSession;

#[cfg(feature = "tor")]
const BRIDGE_DEFAULT_SAMPLE_SIZE: usize = 8;
#[cfg(feature = "tor")]
const BUILTIN_BRIDGES: &[&str] = &[];

#[cfg(feature = "tor")]
pub struct TorTransport {
    client: TorClient<PreferredRuntime>,
    gateway_addr: String,
    tls_config: Arc<rustls::ClientConfig>,
}

#[cfg(feature = "tor")]
impl TorTransport {
    pub async fn bootstrap(
        gateway_addr: String,
        tls_config: Arc<rustls::ClientConfig>,
        settings: TorSettings,
    ) -> Result<Self> {
        let config = tor_config(&settings)?;
        let client = TorClient::create_bootstrapped(config)
            .await
            .map_err(|e| Error::Transport(format!("tor bootstrap failed: {e}")))?;

        Ok(Self {
            client,
            gateway_addr,
            tls_config,
        })
    }

    pub async fn connect_shadow(&self) -> Result<ShadowSession> {
        let addr = self
            .gateway_addr
            .strip_prefix("wss://")
            .or_else(|| self.gateway_addr.strip_prefix("ws://"))
            .unwrap_or(&self.gateway_addr);

        let mut parts = addr.rsplitn(2, ':');
        let port_str = parts
            .next()
            .ok_or_else(|| Error::Transport("missing gateway port".into()))?;
        let host = parts
            .next()
            .ok_or_else(|| Error::Transport("missing gateway host".into()))?;
        let port: u16 = port_str
            .parse()
            .map_err(|e| Error::Transport(format!("invalid gateway port: {e}")))?;

        let tor_stream = self
            .client
            .connect((host, port))
            .await
            .map_err(|e| Error::Transport(format!("tor connect failed: {e}")))?;

        let connector = TlsConnector::from(self.tls_config.clone());
        let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|e| Error::Transport(format!("invalid server name: {e}")))?;

        let tls_stream = connector
            .connect(server_name, tor_stream)
            .await
            .map_err(|e| Error::Transport(format!("tls over tor failed: {e}")))?;

        let session = TransportSession::from_stream(tls_stream);
        let conn = ServerConnection::from_session(session);
        ShadowSession::connect_with_connection(conn).await
    }
}

#[cfg(feature = "tor")]
fn tor_config(settings: &TorSettings) -> Result<TorClientConfig> {
    let mut builder = TorClientConfigBuilder::default();
    let bridge_lines = resolve_bridge_lines(settings);
    if !bridge_lines.is_empty() {
        for line in bridge_lines {
            let bridge: BridgeConfigBuilder = line
                .parse()
                .map_err(|e| Error::Config(format!("invalid tor bridge line '{line}': {e}")))?;
            builder.bridges().bridges().push(bridge);
        }
        builder.bridges().enabled(BoolOrAuto::Explicit(true));
    }

    builder
        .build()
        .map_err(|e| Error::Config(format!("tor config build failed: {e}")))
}

#[cfg(feature = "tor")]
fn resolve_bridge_lines(settings: &TorSettings) -> Vec<String> {
    let mut lines = BUILTIN_BRIDGES
        .iter()
        .map(|line| (*line).to_string())
        .collect::<Vec<_>>();
    lines.extend(settings.bridge_lines.iter().cloned());
    let mut lines = dedup_and_validate(lines);
    if lines.len() > BRIDGE_DEFAULT_SAMPLE_SIZE {
        lines.shuffle(&mut rand::thread_rng());
        lines.truncate(BRIDGE_DEFAULT_SAMPLE_SIZE);
    }
    lines
}

#[cfg(feature = "tor")]
fn dedup_and_validate(lines: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for mut line in lines {
        line = line.trim().to_string();
        if line.is_empty() || line.len() > 1024 || !seen.insert(line.clone()) {
            continue;
        }
        out.push(line);
    }

    out
}

#[cfg(all(test, feature = "tor"))]
mod tests {
    use super::*;

    #[test]
    fn resolve_bridge_lines_uses_runtime_settings() {
        let settings = TorSettings {
            enabled: true,
            bridge_lines: vec!["obfs4 127.0.0.1:1111 abc cert=xyz iat-mode=0".into()],
        };

        let lines = resolve_bridge_lines(&settings);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("obfs4 127.0.0.1:1111"));
    }

    #[test]
    fn resolve_bridge_lines_filters_duplicates() {
        let settings = TorSettings {
            enabled: true,
            bridge_lines: vec![" bridge-a ".into(), "bridge-a".into(), String::new()],
        };

        let lines = resolve_bridge_lines(&settings);
        assert_eq!(lines, vec!["bridge-a".to_string()]);
    }
}
