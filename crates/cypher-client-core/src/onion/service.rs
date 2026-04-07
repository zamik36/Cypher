use std::sync::Arc;

use cypher_common::{Error, Result};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::bootstrap::TransportBootstrap;
use super::config::AnonymousTransportConfig;
use super::cover::CoverTraffic;
use super::fetcher::PipelinedFetcher;
use super::indicator::AnonymityLevel;
use super::pool::TransportPool;

pub struct AnonymousTransportService {
    config: Mutex<AnonymousTransportConfig>,
    bootstrap: TransportBootstrap,
    pool: Arc<TransportPool>,
    cover_cancel: Mutex<Option<CancellationToken>>,
}

impl AnonymousTransportService {
    pub fn new(
        gateway_addr: String,
        tls_config: Arc<rustls::ClientConfig>,
        bootstrap: TransportBootstrap,
        config: AnonymousTransportConfig,
    ) -> Result<Self> {
        if !bootstrap.supports_signed_inbox() {
            return Err(Error::Protocol(
                "transport bootstrap missing signed inbox capability".into(),
            ));
        }

        let pool = Arc::new(
            TransportPool::new(
                gateway_addr,
                bootstrap.relay.clone(),
                config.tor.clone(),
                tls_config,
            )
            .with_target_count(config.target_count),
        );

        Ok(Self {
            config: Mutex::new(config),
            bootstrap,
            pool,
            cover_cancel: Mutex::new(None),
        })
    }

    pub async fn start(&self) {
        self.pool.clone().start_warming().await;
        self.restart_cover().await;
    }

    pub async fn set_config(&self, config: AnonymousTransportConfig) {
        *self.config.lock().await = config;
        self.restart_cover().await;
    }

    pub async fn level(&self) -> AnonymityLevel {
        super::indicator::compute_level(&self.pool).await
    }

    pub async fn fetch_all(&self, inbox_ids: Vec<Vec<u8>>) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let fetcher = PipelinedFetcher::new(self.pool.clone(), self.bootstrap.inbox_verifying_key);
        fetcher.fetch_all(inbox_ids).await
    }

    async fn restart_cover(&self) {
        if let Some(cancel) = self.cover_cancel.lock().await.take() {
            cancel.cancel();
        }

        let config = self.config.lock().await.clone();
        let cancel = CancellationToken::new();
        *self.cover_cancel.lock().await = Some(cancel.clone());

        let cover = CoverTraffic::new(
            self.pool.clone(),
            self.bootstrap.inbox_verifying_key,
            config.power_mode,
        );
        tokio::spawn(async move {
            if let Err(e) = cover.run(cancel).await {
                debug!("cover traffic stopped: {e}");
            }
        });
    }
}
