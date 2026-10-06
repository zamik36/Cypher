//! Signaling: prekeys, share links and blind inboxes behind NATS
//! request/reply. Stateless; all state lives in Redis.

mod handler;
mod onion;
mod push;
mod store;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use async_nats::{Message, Subscriber};
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

/// Requests handled at once, per source. Session requests (keys, links)
/// are small; an onion request may return an inbox batch of up to 768 KiB,
/// so its pool bounds that memory (~48 MiB) and anonymous load cannot crowd
/// out signed-in users. A request beyond its pool is dropped at once rather
/// than stalling the subscription: the gateway answers it as unavailable
/// after its timeout and the client retries with backoff.
const SESSION_PERMITS: usize = 1024;
const ONION_PERMITS: usize = 64;

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
    /// Where the VAPID key for Web Push lives; push is off without it.
    pub vapid_key_path: Option<PathBuf>,
    /// The VAPID `sub` claim: how push services can reach the operator.
    #[serde(default = "default_push_contact")]
    pub push_contact: String,
    /// More push services endpoints may point at, comma-separated (e.g. a
    /// self-hosted `UnifiedPush` distributor).
    #[serde(default)]
    pub push_extra_hosts: String,
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
fn default_push_contact() -> String {
    "https://cyphermessanger.tech".into()
}

/// Web Push from the configured VAPID key, or off.
fn web_push(config: &Config) -> anyhow::Result<Option<handler::Pusher>> {
    let Some(path) = &config.vapid_key_path else {
        info!("no VAPID key path; push notifications are off");
        return Ok(None);
    };
    let extra: Vec<String> = config
        .push_extra_hosts
        .split(',')
        .map(str::to_owned)
        .collect();
    let web = push::WebPush::new(
        secrets::load_or_create_secret(path)?,
        config.push_contact.clone(),
        push::Policy::new(&extra),
    )?;
    Ok(Some(handler::Pusher::Web(Arc::new(web))))
}

struct Service {
    handler: Handler,
    onion_secret: StaticSecret,
    nats: async_nats::Client,
    requests: IntCounter,
    onion_requests: IntCounter,
    shed: IntCounter,
}

impl Service {
    async fn subscribe(&self, subject: &'static str) -> anyhow::Result<Subscriber> {
        Ok(self
            .nats
            .queue_subscribe(subject, SIG_QUEUE_GROUP.to_owned())
            .await?)
    }

    /// Handles `subject` until its subscription ends, at most `permits`
    /// requests at once.
    async fn serve(self: Arc<Self>, subject: &'static str, mut sub: Subscriber, permits: usize) {
        let permits = Arc::new(Semaphore::new(permits));
        while let Some(msg) = sub.next().await {
            let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                self.shed.inc();
                continue;
            };
            let service = Arc::clone(&self);
            tokio::spawn(async move {
                service.dispatch(subject, msg).await;
                drop(permit);
            });
        }
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
        nats = %config.nats.url,
        "signaling starting"
    );

    let onion_secret = StaticSecret::from(secrets::load_or_create_secret(&config.onion_key_path)?);
    let push = web_push(&config)?;
    let relay_addr = config.relay_public_addr.filter(|addr| {
        let valid = cypher_wire::relay_addr_is_valid(addr);
        if !valid {
            warn!(%addr, "relay_public_addr is not a valid host:port; anonymous routing is off");
        }
        valid
    });
    let service = Arc::new(Service {
        handler: Handler {
            store: Store::connect(&config.redis_url).await?,
            onion_public: PublicKey::from(&onion_secret).to_bytes(),
            relay_addr,
            push,
        },
        onion_secret,
        nats: cypher_server_kit::connect_nats(&config.nats).await?,
        requests: registry.counter("signaling_requests_total", "Session requests handled")?,
        onion_requests: registry
            .counter("signaling_onion_requests_total", "Onion requests handled")?,
        shed: registry.counter(
            "signaling_shed_total",
            "Requests dropped because their pool was full",
        )?,
    });

    let sessions = service.subscribe(SIG_REQUEST_SUBJECT).await?;
    let onion = service.subscribe(SIG_ONION_SUBJECT).await?;
    registry.set_ready(service.nats.clone());
    info!("signaling ready");
    tokio::select! {
        () = Arc::clone(&service).serve(SIG_REQUEST_SUBJECT, sessions, SESSION_PERMITS) => {}
        () = Arc::clone(&service).serve(SIG_ONION_SUBJECT, onion, ONION_PERMITS) => {}
        () = shutdown.cancelled() => {}
    }
    let _ = service.nats.flush().await;
    Ok(())
}
