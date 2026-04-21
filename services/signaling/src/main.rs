//! Signaling Service - peer discovery, link management, ICE relay, prekey storage.
//!
//! Listens on NATS for messages forwarded by the Gateway service and uses
//! Redis for ephemeral state:
//!   - link:{link_id}          -> creator peer_id hex   (TTL 24h)
//!   - peer:{peer_id}:prekeys  -> JSON {identity_key, signed_prekey} (TTL session)
//!   - peer:{peer_id}:session  -> JSON {gateway_node, session_id}    (TTL session)
//!   - ice:{peer_a}:{peer_b}   -> JSON [candidates]                  (TTL 5min)

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use futures::StreamExt;
use prometheus::{IntCounter, IntGauge};
use serde::Deserialize;
use tracing::{debug, info, warn};

mod bootstrap;
mod delivery;
mod inbox;
mod key_store;
mod peer;
mod signing;
mod stun;

use signing::ServerSigner;
use stun::StunServer;

static LINKS_CREATED: LazyLock<IntCounter> = LazyLock::new(|| {
    let counter = IntCounter::new("signaling_links_created_total", "Total links created").unwrap();
    let _ = prometheus::register(Box::new(counter.clone()));
    counter
});
static PEER_SESSIONS: LazyLock<IntGauge> = LazyLock::new(|| {
    let gauge = IntGauge::new(
        "signaling_peer_sessions",
        "Number of registered peer sessions",
    )
    .unwrap();
    let _ = prometheus::register(Box::new(gauge.clone()));
    gauge
});

const LINK_TTL_SECS: u64 = 24 * 60 * 60;
const SESSION_TTL_SECS: u64 = 2 * 60 * 60;
const ICE_TTL_SECS: u64 = 5 * 60;
const PREKEY_TTL_SECS: u64 = 2 * 60 * 60;

#[derive(Debug, Deserialize)]
struct GatewayEnvelope {
    session_id: u64,
    payload: Vec<u8>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PeerSession {
    gateway_node: String,
    session_id: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PrekeyBundle {
    identity_key: Vec<u8>,
    signed_prekey: Vec<u8>,
    #[serde(default)]
    inbox_id: Option<String>,
}

struct SignalingService {
    redis: redis::aio::ConnectionManager,
    nats: async_nats::Client,
    node_id: String,
    signer: ServerSigner,
}

impl SignalingService {
    async fn new(
        redis_url: &str,
        nats_url: &str,
        nats_token: Option<&str>,
    ) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url)?;
        let redis = client.get_connection_manager().await?;
        let nats = match nats_token {
            Some(token) if !token.is_empty() => {
                async_nats::ConnectOptions::with_token(token.to_string())
                    .connect(nats_url)
                    .await?
            }
            _ => async_nats::connect(nats_url).await?,
        };

        Ok(Self {
            redis,
            nats,
            node_id: std::env::var("P2P_NODE_ID").unwrap_or_else(|_| "gateway-0".to_string()),
            signer: ServerSigner::load_or_create_default()?,
        })
    }

    async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        let subjects = [
            "signaling.session.register",
            "signaling.session.deregister",
            "signaling.request_peer",
            "signaling.ice_candidate",
            "signaling.offer",
            "signaling.answer",
            "signaling.upload_prekeys",
            "signaling.get_prekeys",
            "signaling.transport_bootstrap",
            "signaling.chat_send",
            "signaling.create_link",
            "signaling.file_offer",
            "signaling.file_accept",
            "signaling.file_chunk",
            "signaling.file_complete",
            "signaling.file_chunk_ack",
            "signaling.file_resume",
            "signaling.inbox_store",
            "signaling.inbox_fetch",
            "signaling.inbox_ack",
            "signaling.data",
            "signaling.raw",
        ];

        let mut subscribers = Vec::new();
        for subject in &subjects {
            let subscription = self.nats.subscribe(subject.to_string()).await?;
            subscribers.push((*subject, subscription));
        }

        info!(
            "Signaling service listening on {} NATS subjects",
            subjects.len()
        );

        let mut handles = Vec::new();
        for (subject, mut subscription) in subscribers {
            let service = self.clone();
            let subject_owned = subject.to_string();
            let handle = tokio::spawn(async move {
                while let Some(msg) = subscription.next().await {
                    let service = service.clone();
                    let subject = subject_owned.clone();
                    tokio::spawn(async move {
                        if let Err(error) = service.handle_message(&subject, &msg).await {
                            warn!(subject = %subject, "error handling message: {}", error);
                        }
                    });
                }
            });
            handles.push(handle);
        }

        futures::future::join_all(handles).await;
        Ok(())
    }

    async fn handle_message(&self, subject: &str, msg: &async_nats::Message) -> anyhow::Result<()> {
        match subject {
            "signaling.session.register" => self.handle_session_register(msg).await,
            "signaling.session.deregister" => self.handle_session_deregister(msg).await,
            "signaling.request_peer" => self.handle_request_peer(msg).await,
            "signaling.ice_candidate" => self.handle_ice_candidate(msg).await,
            "signaling.offer" => self.handle_offer(msg).await,
            "signaling.answer" => self.handle_answer(msg).await,
            "signaling.upload_prekeys" => self.handle_upload_prekeys(msg).await,
            "signaling.get_prekeys" => self.handle_get_prekeys(msg).await,
            "signaling.transport_bootstrap" => self.handle_transport_bootstrap(msg).await,
            "signaling.chat_send" => self.handle_chat_send(msg).await,
            "signaling.create_link" => self.handle_create_link(msg).await,
            "signaling.file_offer" => self.handle_file_forward(msg, "file.offer").await,
            "signaling.file_accept" => self.handle_file_forward(msg, "file.accept").await,
            "signaling.file_chunk" => self.handle_file_forward(msg, "file.chunk").await,
            "signaling.file_complete" => self.handle_file_forward(msg, "file.complete").await,
            "signaling.file_chunk_ack" => self.handle_file_forward(msg, "file.chunkAck").await,
            "signaling.file_resume" => self.handle_file_forward(msg, "file.resume").await,
            "signaling.inbox_store" => self.handle_inbox_store(msg).await,
            "signaling.inbox_fetch" => self.handle_inbox_fetch(msg).await,
            "signaling.inbox_ack" => self.handle_inbox_ack(msg).await,
            _ => {
                debug!(subject, "unhandled signaling message");
                Ok(())
            }
        }
    }
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn hex_decode_bytes(hex: &str) -> Vec<u8> {
    if !hex.len().is_multiple_of(2) {
        return Vec::new();
    }
    (0..hex.len())
        .step_by(2)
        .filter_map(|offset| u8::from_str_radix(&hex[offset..offset + 2], 16).ok())
        .collect()
}

static LOG_SALT: LazyLock<[u8; 16]> = LazyLock::new(|| {
    let mut salt = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
    salt
});

fn short_id(raw: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&*LOG_SALT).expect("HMAC accepts any key size");
    mac.update(raw.as_bytes());
    let result = mac.finalize().into_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        result[0], result[1], result[2], result[3]
    )
}

fn redact_secret_url(raw: &str) -> String {
    let Some(scheme_sep) = raw.find("://") else {
        return raw.to_string();
    };
    let authority_start = scheme_sep + 3;
    let Some(authority_end) = raw[authority_start..].find('@') else {
        return raw.to_string();
    };
    let authority_end = authority_start + authority_end;
    let userinfo = &raw[authority_start..authority_end];

    if let Some(password_sep) = userinfo.find(':') {
        let username = &userinfo[..password_sep];
        let mut redacted = String::with_capacity(raw.len());
        redacted.push_str(&raw[..authority_start]);
        redacted.push_str(username);
        redacted.push_str(":***");
        redacted.push_str(&raw[authority_end..]);
        return redacted;
    }

    raw.to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_common::init_tracing();
    let config = cypher_common::AppConfig::load()?;

    cypher_common::metrics::spawn_metrics_server(9091);

    info!("Signaling service starting");
    info!("  Redis: {}", redact_secret_url(&config.redis_url));
    info!("  NATS:  {}", config.nats_url);
    info!("  STUN:  {}", config.stun_addr);

    let stun_addr: SocketAddr = config.stun_addr.parse()?;
    let stun = StunServer::bind(stun_addr).await?;
    tokio::spawn(async move {
        stun.run().await;
    });

    let nats_token = std::env::var("P2P_NATS_TOKEN").ok();
    let service = Arc::new(
        SignalingService::new(&config.redis_url, &config.nats_url, nats_token.as_deref()).await?,
    );
    {
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(error) = service.recover_claims().await {
                warn!("inbox claim recovery stopped: {error}");
            }
        });
    }

    info!("Signaling service running");
    service.run().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{redact_secret_url, short_id};

    #[test]
    fn redacts_password_in_authority() {
        let raw = "redis://:super-secret@redis:6379";
        assert_eq!(redact_secret_url(raw), "redis://:***@redis:6379");
    }

    #[test]
    fn leaves_passwordless_urls_unchanged() {
        let raw = "nats://nats:4222";
        assert_eq!(redact_secret_url(raw), raw);
    }

    #[test]
    fn pseudonym_is_deterministic_within_session_and_hides_original() {
        let id = "abcdef1234567890abcdef1234567890";
        let p1 = short_id(id);
        let p2 = short_id(id);
        assert_eq!(p1, p2);
        assert_eq!(p1.len(), 8);
        assert!(!id.contains(&p1));
    }

    #[test]
    fn different_ids_produce_different_pseudonyms() {
        let a = short_id("aaaa1111bbbb2222cccc3333dddd4444");
        let b = short_id("eeee5555ffff6666777788889999aaaa");
        assert_ne!(a, b);
    }
}
