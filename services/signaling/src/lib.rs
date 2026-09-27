//! Signaling: prekeys, share links and blind inboxes behind NATS
//! request/reply. Stateless; all state lives in Redis.

mod handler;
mod onion;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use async_nats::Message;
use cypher_server_kit::metrics::Metrics;
use cypher_server_kit::{
    PEER_HEADER, SIG_ONION_SUBJECT, SIG_QUEUE_GROUP, SIG_REQUEST_SUBJECT, secrets,
};
use cypher_types::PeerId;
use futures::StreamExt;
use prometheus::IntCounter;
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use x25519_dalek::{PublicKey, StaticSecret};

use handler::Handler;
use store::Store;

const MAX_IN_FLIGHT: usize = 4096;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    #[serde(flatten)]
    pub nats: cypher_server_kit::NatsConfig,
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: SocketAddr,
    #[serde(default = "default_onion_key_path")]
    pub onion_key_path: PathBuf,
    /// Public `host:port` of the onion relay advertised to clients.
    pub relay_public_addr: Option<String>,
}

fn default_redis_url() -> String {
    "redis://127.0.0.1:6379".into()
}
fn default_metrics_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9091))
}
fn default_onion_key_path() -> PathBuf {
    PathBuf::from("data/onion_key.bin")
}

struct Service {
    handler: Handler,
    onion_secret: StaticSecret,
    nats: async_nats::Client,
    permits: Arc<Semaphore>,
    requests: IntCounter,
    onion_requests: IntCounter,
}

impl Service {
    async fn serve(self: Arc<Self>, subject: &'static str) -> anyhow::Result<()> {
        let mut sub = self
            .nats
            .queue_subscribe(subject, SIG_QUEUE_GROUP.to_owned())
            .await?;
        while let Some(msg) = sub.next().await {
            let permit = Arc::clone(&self.permits).acquire_owned().await?;
            let service = Arc::clone(&self);
            tokio::spawn(async move {
                service.dispatch(subject, msg).await;
                drop(permit);
            });
        }
        Ok(())
    }

    async fn dispatch(&self, subject: &str, msg: Message) {
        let Some(reply_to) = msg.reply.clone() else {
            return;
        };
        let reply = if subject == SIG_ONION_SUBJECT {
            self.onion_requests.inc();
            onion::handle(&self.handler, &self.onion_secret, &msg.payload).await
        } else {
            self.requests.inc();
            let peer = msg
                .headers
                .as_ref()
                .and_then(|h| h.get(PEER_HEADER))
                .and_then(|v| PeerId::from_hex(v.as_str()));
            Some(self.handler.handle(peer, msg.payload.clone()).await)
        };
        if let Some(reply) = reply
            && let Err(e) = self.nats.publish(reply_to, reply).await
        {
            warn!("failed to publish reply: {e}");
        }
    }
}

/// Serves signaling requests from NATS until `shutdown` is cancelled.
pub async fn run(config: Config, shutdown: CancellationToken) -> anyhow::Result<()> {
    let registry = Metrics::new()?;
    registry
        .serve(config.metrics_addr, shutdown.clone())
        .await?;

    info!(
        redis = %cypher_server_kit::redact_url(&config.redis_url),
        nats = %config.nats.nats_url,
        "signaling starting"
    );

    let onion_secret = StaticSecret::from(secrets::load_or_create_secret(&config.onion_key_path)?);
    let service = Arc::new(Service {
        handler: Handler {
            store: Store::connect(&config.redis_url).await?,
            onion_public: PublicKey::from(&onion_secret).to_bytes(),
            relay_addr: config
                .relay_public_addr
                .filter(|a| cypher_wire::relay_addr_is_valid(a)),
        },
        onion_secret,
        nats: cypher_server_kit::connect_nats(&config.nats).await?,
        permits: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        requests: registry.counter("signaling_requests_total", "Session requests handled")?,
        onion_requests: registry
            .counter("signaling_onion_requests_total", "Onion requests handled")?,
    });

    info!("signaling ready");
    tokio::select! {
        r = Arc::clone(&service).serve(SIG_REQUEST_SUBJECT) => r?,
        r = Arc::clone(&service).serve(SIG_ONION_SUBJECT) => r?,
        () = shutdown.cancelled() => {}
    }
    let _ = service.nats.flush().await;
    Ok(())
}
