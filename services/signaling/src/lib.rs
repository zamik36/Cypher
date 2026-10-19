//! Signaling: prekeys, share links and blind inboxes behind NATS
//! request/reply. Stateless; all state lives in Redis.

mod handler;
mod keys;
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
    PEER_HEADER, SIG_KEYS_SUBJECT, SIG_ONION_SUBJECT, SIG_QUEUE_GROUP, SIG_REQUEST_SUBJECT, secrets,
};
use futures::{Stream, StreamExt};
use prometheus::IntCounter;
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use handler::Handler;
use keys::OnionKeys;
use store::Store;

/// Requests handled at once, per source, so anonymous load cannot crowd
/// out signed-in users. Session requests (keys, links) are small. Onion
/// requests are many (every sealed message is one, waiting on the
/// recipient's gateway) and up to ~100 KiB each; inbox fetches, whose
/// batches are larger, have a pool of their own within
/// ([`handler::FETCH_PERMITS`]). A request beyond its pool is dropped at
/// once rather than stalling the subscription: the gateway answers it as
/// unavailable after its timeout and the client retries with backoff.
const SESSION_PERMITS: usize = 1024;
const ONION_PERMITS: usize = 512;
/// Key sets are small and the relay caches them.
const KEY_SET_PERMITS: usize = 16;
/// How often the onion keys are looked at again: a new epoch's key is in
/// place long before clients seal to it.
const KEY_REFRESH: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    #[serde(flatten)]
    pub nats: cypher_server_kit::NatsConfig,
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: SocketAddr,
    /// The long-term key clients remember this server by; it signs the onion
    /// keys. Back it up: clients refuse a server whose key changed.
    #[serde(default = "default_signing_key_path")]
    pub signing_key_path: PathBuf,
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
fn default_signing_key_path() -> PathBuf {
    PathBuf::from("data/signing_key.bin")
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
    keys: OnionKeys,
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
    /// requests at once; why it ended is returned.
    async fn serve(
        self: Arc<Self>,
        subject: &'static str,
        sub: Subscriber,
        permits: usize,
    ) -> anyhow::Error {
        let service = Arc::clone(&self);
        serve(subject, sub, permits, &self.shed, move |msg| {
            let service = Arc::clone(&service);
            async move { service.dispatch(subject, msg).await }
        })
        .await
    }

    async fn dispatch(&self, subject: &str, msg: Message) {
        let Some(reply_to) = msg.reply.clone() else {
            return;
        };
        let reply = if subject == SIG_KEYS_SUBJECT {
            self.keys.key_set(now_secs()).map(bytes::Bytes::from)
        } else if subject == SIG_ONION_SUBJECT {
            self.onion_requests.inc();
            onion::handle(&self.handler, &self.keys, &msg.payload).await
        } else {
            self.requests.inc();
            let peer = msg
                .headers
                .as_ref()
                .and_then(|h| h.get(PEER_HEADER))
                .map(async_nats::HeaderValue::as_str);
            Some(
                self.handler
                    .handle_forwarded(peer, msg.payload.clone())
                    .await,
            )
        };
        if let Some(reply) = reply
            && let Err(e) = self.nats.publish(reply_to, reply).await
        {
            warn!("failed to publish reply: {e}");
        }
    }
}

/// Hands each message of `sub` to its own task from `handle`, at most
/// `permits` at once; what comes beyond is dropped and counted in `shed`.
/// A NATS subscription ends only once its connection is gone for good, so
/// its end is an error: the service stops instead of idling unseen.
async fn serve<T, F, Fut>(
    subject: &'static str,
    mut sub: impl Stream<Item = T> + Unpin,
    permits: usize,
    shed: &IntCounter,
    handle: F,
) -> anyhow::Error
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let permits = Arc::new(Semaphore::new(permits));
    while let Some(msg) = sub.next().await {
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            shed.inc();
            continue;
        };
        let task = handle(msg);
        tokio::spawn(async move {
            task.await;
            drop(permit);
        });
    }
    anyhow::anyhow!("the subscription to {subject} ended")
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Keeps the onion keys for the current and the next epoch in place until
/// `shutdown`.
async fn refresh_keys(service: Arc<Service>, shutdown: CancellationToken) {
    let mut every = tokio::time::interval(KEY_REFRESH);
    every.tick().await;
    while tokio::select! {
        _ = every.tick() => true,
        () = shutdown.cancelled() => false,
    } {
        if let Err(e) = service
            .keys
            .refresh(&service.handler.store, now_secs())
            .await
        {
            warn!("refreshing the onion keys failed: {e}");
        }
    }
}

/// The relay address clients are told of, if it is a valid `host:port`;
/// anonymous routing is off without one.
fn relay_public_addr(configured: Option<String>) -> Option<String> {
    configured.filter(|addr| {
        let valid = cypher_wire::relay_addr_is_valid(addr);
        if !valid {
            warn!(%addr, "relay_public_addr is not a valid host:port; anonymous routing is off");
        }
        valid
    })
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

    let keys = OnionKeys::new(&secrets::load_or_create_secret(&config.signing_key_path)?);
    let push = web_push(&config)?;
    let relay_addr = relay_public_addr(config.relay_public_addr);
    let nats = cypher_server_kit::connect_nats(&config.nats).await?;
    let service = Arc::new(Service {
        handler: Handler {
            store: Store::connect(&config.redis_url).await?,
            signing_key: keys.signing_key(),
            relay_addr,
            push,
            courier: handler::Courier::Nats(nats.clone()),
            fetches: Semaphore::new(handler::FETCH_PERMITS),
            sealed_unavailable: registry.counter(
                "signaling_sealed_unavailable_total",
                "Sealed sends no gateway answered in time",
            )?,
            inbox_quota_refused: registry.counter(
                "signaling_inbox_quota_refused_total",
                "Inbox writes and openings refused because all inboxes together are full",
            )?,
        },
        keys,
        nats,
        requests: registry.counter("signaling_requests_total", "Session requests handled")?,
        onion_requests: registry
            .counter("signaling_onion_requests_total", "Onion requests handled")?,
        shed: registry.counter(
            "signaling_shed_total",
            "Requests dropped because their pool was full",
        )?,
    });

    service
        .keys
        .refresh(&service.handler.store, now_secs())
        .await?;
    let sessions = service.subscribe(SIG_REQUEST_SUBJECT).await?;
    let onion = service.subscribe(SIG_ONION_SUBJECT).await?;
    let key_sets = service.subscribe(SIG_KEYS_SUBJECT).await?;
    registry.set_ready(service.nats.clone());
    info!("signaling ready");
    let ended = tokio::select! {
        e = Arc::clone(&service).serve(SIG_REQUEST_SUBJECT, sessions, SESSION_PERMITS) => Some(e),
        e = Arc::clone(&service).serve(SIG_ONION_SUBJECT, onion, ONION_PERMITS) => Some(e),
        e = Arc::clone(&service).serve(SIG_KEYS_SUBJECT, key_sets, KEY_SET_PERMITS) => Some(e),
        () = refresh_keys(Arc::clone(&service), shutdown.clone()) => None,
        () = shutdown.cancelled() => None,
    };
    let _ = service.nats.flush().await;
    stopped(ended, &shutdown)
}

/// How `run` ends: a subscription that `ended` on its own is a failure,
/// one that ended because of `shutdown` is not.
fn stopped(ended: Option<anyhow::Error>, shutdown: &CancellationToken) -> anyhow::Result<()> {
    match ended {
        Some(e) if !shutdown.is_cancelled() => Err(e),
        Some(_) | None => Ok(()),
    }
}
