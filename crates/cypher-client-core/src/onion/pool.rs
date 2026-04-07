use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, warn};

use cypher_common::Result;

use super::bootstrap::RelayBootstrap;
use super::circuit::Circuit;
use super::config::TorSettings;
use super::relay_client::RelayClient;
use super::shadow::ShadowSession;
#[cfg(feature = "tor")]
use super::tor::TorTransport;

pub enum TransportHandle {
    #[cfg(feature = "tor")]
    Tor(ShadowSession),
    Relay {
        client: RelayClient,
        circuit: Circuit,
    },
    Direct(ShadowSession),
}

pub struct TransportPool {
    relay_circuits: Mutex<Vec<(RelayClient, Circuit)>>,
    gateway_addr: String,
    relay: Option<RelayBootstrap>,
    #[cfg(feature = "tor")]
    tor_settings: TorSettings,
    tls_config: Arc<rustls::ClientConfig>,
    target_count: usize,
    #[cfg(feature = "tor")]
    tor_transport: Mutex<Option<Arc<TorTransport>>>,
}

impl TransportPool {
    pub fn new(
        gateway_addr: String,
        relay: Option<RelayBootstrap>,
        tor_settings: TorSettings,
        tls_config: Arc<rustls::ClientConfig>,
    ) -> Self {
        #[cfg(not(feature = "tor"))]
        let _ = &tor_settings;
        Self {
            relay_circuits: Mutex::new(Vec::new()),
            gateway_addr,
            relay,
            #[cfg(feature = "tor")]
            tor_settings,
            tls_config,
            target_count: 3,
            #[cfg(feature = "tor")]
            tor_transport: Mutex::new(None),
        }
    }

    pub fn with_target_count(mut self, target_count: usize) -> Self {
        self.target_count = target_count.max(1);
        self
    }

    pub async fn start_warming(self: Arc<Self>) {
        #[cfg(feature = "tor")]
        if self.tor_settings.enabled {
            let me = self.clone();
            tokio::spawn(async move {
                match TorTransport::bootstrap(
                    me.gateway_addr.clone(),
                    me.tls_config.clone(),
                    me.tor_settings.clone(),
                )
                .await
                {
                    Ok(tor) => {
                        *me.tor_transport.lock().await = Some(Arc::new(tor));
                        debug!("transport pool: tor bootstrapped");
                    }
                    Err(e) => warn!("transport pool: tor bootstrap failed: {e}"),
                }
            });
        }

        if self.relay.is_some() {
            for _ in 0..self.target_count {
                let me = self.clone();
                tokio::spawn(async move {
                    let _ = me.warm_one().await;
                });
            }
        }
    }

    async fn warm_one(&self) -> Result<()> {
        let Some(relay) = self.relay.as_ref() else {
            return Ok(());
        };
        let client = RelayClient::connect(&relay.addr, self.tls_config.clone()).await?;
        let circuit = Circuit::new(&relay.public_key);

        self.relay_circuits.lock().await.push((client, circuit));
        debug!("transport pool: relay circuit warmed");
        Ok(())
    }

    pub async fn acquire(self: &Arc<Self>) -> Result<TransportHandle> {
        #[cfg(feature = "tor")]
        if let Some(tor) = self.tor_transport.lock().await.clone() {
            match tor.connect_shadow().await {
                Ok(session) => return Ok(TransportHandle::Tor(session)),
                Err(e) => warn!("transport pool: tor acquire failed: {e}"),
            }
        }

        let remaining = {
            let mut circuits = self.relay_circuits.lock().await;
            if let Some(pair) = circuits.pop() {
                let remaining = circuits.len();
                drop(circuits);
                // Replenish in background if pool is getting low.
                if remaining < self.target_count && self.relay.is_some() {
                    let me = self.clone();
                    tokio::spawn(async move {
                        let _ = me.warm_one().await;
                    });
                }
                return Ok(TransportHandle::Relay {
                    client: pair.0,
                    circuit: pair.1,
                });
            }
            0
        };

        // No relay circuits available — start warming and fall back to direct.
        if remaining == 0 && self.relay.is_some() {
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.warm_one().await;
            });
        }

        match ShadowSession::connect(&self.gateway_addr, self.tls_config.clone()).await {
            Ok(session) => Ok(TransportHandle::Direct(session)),
            Err(e) => {
                warn!("transport pool: failed to create direct shadow session: {e}");
                Err(e)
            }
        }
    }

    pub async fn release(&self, handle: TransportHandle) {
        match handle {
            #[cfg(feature = "tor")]
            TransportHandle::Tor(session) => {
                // Tor sessions are one-shot; always close after use.
                tokio::spawn(async move {
                    let _ = session.close().await;
                });
            }
            TransportHandle::Relay {
                mut client,
                circuit,
            } => {
                let mut circuits = self.relay_circuits.lock().await;
                if circuits.len() < self.target_count {
                    circuits.push((client, circuit));
                } else {
                    drop(circuits);
                    tokio::spawn(async move {
                        let _ = client.close().await;
                    });
                }
            }
            TransportHandle::Direct(session) => {
                // Direct sessions are ephemeral; always close after use
                // to avoid keeping idle connections to the gateway.
                tokio::spawn(async move {
                    let _ = session.close().await;
                });
            }
        }
    }

    pub async fn relay_ready_count(&self) -> usize {
        self.relay_circuits.lock().await.len()
    }

    #[cfg(feature = "tor")]
    pub async fn tor_ready_count(&self) -> usize {
        usize::from(self.tor_transport.lock().await.is_some())
    }
}
