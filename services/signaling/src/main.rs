//! Signaling Service - peer discovery, link management, ICE relay, prekey storage.
//!
//! Listens on NATS for messages forwarded by the Gateway service and uses
//! Redis for ephemeral state:
//!   - link:{link_id}          -> creator peer_id hex   (TTL 24h)
//!   - peer:{peer_id}:prekeys  -> JSON {identity_key, signed_prekey} (TTL session)
//!   - peer:{peer_id}:session  -> JSON {gateway_node, session_id}    (TTL session)
//!   - ice:{peer_a}:{peer_b}   -> JSON [candidates]                  (TTL 5min)

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use prometheus::{IntCounter, IntGauge};
use std::sync::LazyLock;

use cypher_common::LinkId;
use cypher_proto::{dispatch, Message, Serializable};

static LINKS_CREATED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = IntCounter::new("signaling_links_created_total", "Total links created").unwrap();
    let _ = prometheus::register(Box::new(c.clone()));
    c
});
static PEER_SESSIONS: LazyLock<IntGauge> = LazyLock::new(|| {
    let g = IntGauge::new(
        "signaling_peer_sessions",
        "Number of registered peer sessions",
    )
    .unwrap();
    let _ = prometheus::register(Box::new(g.clone()));
    g
});

/// TTL constants for Redis keys.
const LINK_TTL_SECS: u64 = 24 * 60 * 60; // 24 hours
const SESSION_TTL_SECS: u64 = 2 * 60 * 60; // 2 hours
const ICE_TTL_SECS: u64 = 5 * 60; // 5 minutes
const PREKEY_TTL_SECS: u64 = 2 * 60 * 60; // 2 hours

const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_RESPONSE: u16 = 0x0101;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_HEADER_SIZE: usize = 20;

#[derive(Debug, Deserialize)]
struct GatewayEnvelope {
    session_id: u64,
    payload: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PeerSession {
    gateway_node: String,
    session_id: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PrekeyBundle {
    identity_key: Vec<u8>,
    signed_prekey: Vec<u8>,
}

/// RFC 5389 STUN server — responds to Binding Requests with the
/// client's server-reflexive address (XOR-MAPPED-ADDRESS attribute).
pub struct StunServer {
    socket: Arc<UdpSocket>,
}

impl StunServer {
    /// Bind a UDP socket and create a new STUN server.
    pub async fn bind(addr: SocketAddr) -> anyhow::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        info!("STUN server listening on {}", addr);
        Ok(Self {
            socket: Arc::new(socket),
        })
    }

    /// Run forever, responding to STUN Binding Requests.
    pub async fn run(&self) -> ! {
        let mut buf = [0u8; 576];
        loop {
            let (len, from) = match self.socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("STUN recv error: {}", e);
                    continue;
                }
            };

            let data = &buf[..len];

            // Validate minimum header size.
            if data.len() < STUN_HEADER_SIZE {
                debug!(%from, "STUN: datagram too short ({}B), ignoring", len);
                continue;
            }

            let msg_type = u16::from_be_bytes([data[0], data[1]]);
            let cookie = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);

            // Only handle Binding Requests with the correct magic cookie.
            if msg_type != STUN_BINDING_REQUEST || cookie != STUN_MAGIC_COOKIE {
                debug!(%from, msg_type, "STUN: ignoring non-Binding-Request");
                continue;
            }

            // Echo the transaction ID back (bytes 8..20).
            let transaction_id = &data[8..20];

            let response = match build_binding_response(transaction_id, from) {
                Some(r) => r,
                None => {
                    debug!(%from, "STUN: unsupported address family, ignoring");
                    continue;
                }
            };

            if let Err(e) = self.socket.send_to(&response, from).await {
                warn!(%from, "STUN send error: {}", e);
            } else {
                debug!(%from, "STUN: sent Binding Response");
            }
        }
    }
}

/// Build a STUN Binding Response containing an XOR-MAPPED-ADDRESS for the
/// given peer address.  Supports both IPv4 and IPv6.
fn build_binding_response(transaction_id: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
    let x_port = peer.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);

    let attr_value = match peer {
        SocketAddr::V4(v4) => {
            // XOR-MAPPED-ADDRESS value: [0x00][family][x-port(2)][x-addr(4)] = 8 bytes.
            let addr_u32: u32 = u32::from(*v4.ip());
            let x_addr = addr_u32 ^ STUN_MAGIC_COOKIE;

            let mut v = Vec::with_capacity(8);
            v.push(0x00); // reserved
            v.push(0x01); // IPv4 family
            v.extend_from_slice(&x_port.to_be_bytes());
            v.extend_from_slice(&x_addr.to_be_bytes());
            v
        }
        SocketAddr::V6(v6) => {
            // XOR-MAPPED-ADDRESS value: [0x00][family][x-port(2)][x-addr(16)] = 20 bytes.
            // IPv6 XOR key = magic_cookie(4) || transaction_id(12).
            let mut xor_key = [0u8; 16];
            xor_key[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
            xor_key[4..16].copy_from_slice(transaction_id);

            let addr_bytes = v6.ip().octets();
            let mut x_addr = [0u8; 16];
            for i in 0..16 {
                x_addr[i] = addr_bytes[i] ^ xor_key[i];
            }

            let mut v = Vec::with_capacity(20);
            v.push(0x00); // reserved
            v.push(0x02); // IPv6 family
            v.extend_from_slice(&x_port.to_be_bytes());
            v.extend_from_slice(&x_addr);
            v
        }
    };

    // Attribute: type(2) + length(2) + value.
    let attr_len = attr_value.len() as u16;
    let msg_attrs_len = 4 + attr_value.len(); // type + length fields + value

    let mut msg = Vec::with_capacity(STUN_HEADER_SIZE + msg_attrs_len);
    // Message type.
    msg.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
    // Message length (attributes only, not the header).
    msg.extend_from_slice(&(msg_attrs_len as u16).to_be_bytes());
    // Magic cookie.
    msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    // Transaction ID (12 bytes).
    msg.extend_from_slice(transaction_id);
    // XOR-MAPPED-ADDRESS attribute.
    msg.extend_from_slice(&STUN_ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    msg.extend_from_slice(&attr_len.to_be_bytes());
    msg.extend_from_slice(&attr_value);

    Some(msg)
}

/// The signaling service state.
struct SignalingService {
    redis: redis::aio::ConnectionManager,
    nats: async_nats::Client,
    /// Identifier for this gateway node (used in session routing).
    node_id: String,
}

impl SignalingService {
    async fn new(redis_url: &str, nats_url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url)?;
        let redis = client.get_connection_manager().await?;
        let nats = async_nats::connect(nats_url).await?;

        Ok(Self {
            redis,
            nats,
            node_id: std::env::var("P2P_NODE_ID").unwrap_or_else(|_| "gateway-0".to_string()),
        })
    }

    /// Subscribe to all relevant NATS subjects and process messages.
    async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        // Subscribe to all signaling.* subjects.
        let subjects = [
            "signaling.session.register",
            "signaling.request_peer",
            "signaling.ice_candidate",
            "signaling.offer",
            "signaling.answer",
            "signaling.upload_prekeys",
            "signaling.get_prekeys",
            "signaling.chat_send",
            "signaling.create_link",
            "signaling.file_offer",
            "signaling.file_accept",
            "signaling.file_chunk",
            "signaling.file_complete",
            "signaling.file_chunk_ack",
            "signaling.file_resume",
            "signaling.data",
            "signaling.raw",
        ];

        let mut subscribers = Vec::new();
        for subject in &subjects {
            let sub = self.nats.subscribe(subject.to_string()).await?;
            subscribers.push((*subject, sub));
        }

        info!(
            "Signaling service listening on {} NATS subjects",
            subjects.len()
        );

        // Process messages from all subscriptions concurrently.
        let mut handles = Vec::new();
        for (subject, mut sub) in subscribers {
            let svc = self.clone();
            let subject_owned = subject.to_string();
            let handle = tokio::spawn(async move {
                while let Some(msg) = sub.next().await {
                    let svc = svc.clone();
                    let subj = subject_owned.clone();
                    tokio::spawn(async move {
                        if let Err(e) = svc.handle_message(&subj, &msg).await {
                            warn!(subject = %subj, "error handling message: {}", e);
                        }
                    });
                }
            });
            handles.push(handle);
        }

        // Wait for all subscription handlers (they run forever).
        futures::future::join_all(handles).await;
        Ok(())
    }

    /// Dispatch a single NATS message based on its subject.
    async fn handle_message(&self, subject: &str, msg: &async_nats::Message) -> anyhow::Result<()> {
        match subject {
            "signaling.session.register" => self.handle_session_register(msg).await,
            "signaling.request_peer" => self.handle_request_peer(msg).await,
            "signaling.ice_candidate" => self.handle_ice_candidate(msg).await,
            "signaling.offer" => self.handle_offer(msg).await,
            "signaling.answer" => self.handle_answer(msg).await,
            "signaling.upload_prekeys" => self.handle_upload_prekeys(msg).await,
            "signaling.get_prekeys" => self.handle_get_prekeys(msg).await,
            "signaling.chat_send" => self.handle_chat_send(msg).await,
            "signaling.create_link" => self.handle_create_link(msg).await,
            "signaling.file_offer" => self.handle_file_forward(msg, "file.offer").await,
            "signaling.file_accept" => self.handle_file_forward(msg, "file.accept").await,
            "signaling.file_chunk" => self.handle_file_forward(msg, "file.chunk").await,
            "signaling.file_complete" => self.handle_file_forward(msg, "file.complete").await,
            "signaling.file_chunk_ack" => self.handle_file_forward(msg, "file.chunkAck").await,
            "signaling.file_resume" => self.handle_file_forward(msg, "file.resume").await,
            _ => {
                debug!(subject, "unhandled signaling message");
                Ok(())
            }
        }
    }

    async fn handle_session_register(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        #[derive(Deserialize)]
        struct RegisterMsg {
            session_id: u64,
            peer_id: String,
        }

        let reg: RegisterMsg = serde_json::from_slice(&msg.payload)?;
        let key = format!("peer:{}:session", reg.peer_id);
        let session = PeerSession {
            gateway_node: self.node_id.clone(),
            session_id: reg.session_id,
        };
        let value = serde_json::to_string(&session)?;

        let mut redis = self.redis.clone();
        redis
            .set_ex::<_, _, ()>(&key, &value, SESSION_TTL_SECS)
            .await?;

        // Reverse index: session_id -> peer_id for fast lookup.
        let reverse_key = format!("session:{}:peer", reg.session_id);
        redis
            .set_ex::<_, _, ()>(&reverse_key, &reg.peer_id, SESSION_TTL_SECS)
            .await?;

        PEER_SESSIONS.inc();
        info!(peer_id = %reg.peer_id, session_id = reg.session_id, "registered peer session");
        Ok(())
    }

    async fn handle_request_peer(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::SignalRequestPeer(req) = proto_msg {
            let link_key = format!("link:{}", req.link_id);
            let mut redis = self.redis.clone();
            let creator_peer_id: Option<String> = redis.get(&link_key).await?;

            match creator_peer_id {
                Some(peer_id_hex) => {
                    // Look up the creator's session info.
                    let session_key = format!("peer:{}:session", peer_id_hex);
                    let session_json: Option<String> = redis.get(&session_key).await?;

                    let response = serde_json::json!({
                        "found": true,
                        "peer_id": peer_id_hex,
                        "session": session_json.and_then(|s| serde_json::from_str::<PeerSession>(&s).ok()),
                    });

                    // Send response back to the requesting peer's gateway session.
                    let reply_subject = format!("gateway.session.{}", envelope.session_id);
                    self.nats
                        .publish(reply_subject, Bytes::from(response.to_string()))
                        .await?;

                    // Notify the link creator that a peer has joined.
                    if let Ok(joiner_hex) = self.get_peer_id_for_session(envelope.session_id).await
                    {
                        let notification = serde_json::json!({
                            "peer_joined": true,
                            "peer_id": joiner_hex,
                        });
                        if let Err(e) = self
                            .forward_to_peer(&peer_id_hex, notification.to_string().as_bytes())
                            .await
                        {
                            debug!(err = %e, "could not notify link creator of peer join");
                        }
                    }

                    info!(link_id = %req.link_id, "peer found for link");
                }
                None => {
                    let response = serde_json::json!({
                        "found": false,
                        "error": "link not found or expired",
                    });
                    let reply_subject = format!("gateway.session.{}", envelope.session_id);
                    self.nats
                        .publish(reply_subject, Bytes::from(response.to_string()))
                        .await?;

                    debug!(link_id = %req.link_id, "link not found");
                }
            }
        }

        Ok(())
    }

    async fn handle_ice_candidate(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::SignalIceCandidate(ice) = proto_msg {
            let target_peer_hex = hex_encode(&ice.peer_id);

            // Look up the source peer_id from the session.
            let source_peer_hex = self.get_peer_id_for_session(envelope.session_id).await?;

            // Store ICE candidate in Redis.
            let ice_key = format!("ice:{}:{}", source_peer_hex, target_peer_hex);
            let mut redis = self.redis.clone();
            let _: () = redis.rpush(&ice_key, &ice.candidate).await?;
            let _: () = redis.expire(&ice_key, ICE_TTL_SECS as i64).await?;

            // Forward the ICE candidate to the target peer via their gateway session.
            self.forward_to_peer(&target_peer_hex, &envelope.payload)
                .await?;

            debug!(
                target_peer = %target_peer_hex,
                "forwarded ICE candidate"
            );
        }

        Ok(())
    }

    async fn handle_offer(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::SignalOffer(offer) = proto_msg {
            let target_peer_hex = hex_encode(&offer.peer_id);
            self.forward_to_peer(&target_peer_hex, &envelope.payload)
                .await?;
            debug!(target_peer = %target_peer_hex, "forwarded SDP offer");
        }

        Ok(())
    }

    async fn handle_answer(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::SignalAnswer(answer) = proto_msg {
            let target_peer_hex = hex_encode(&answer.peer_id);
            self.forward_to_peer(&target_peer_hex, &envelope.payload)
                .await?;
            debug!(target_peer = %target_peer_hex, "forwarded SDP answer");
        }

        Ok(())
    }

    async fn handle_upload_prekeys(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::KeysUploadPrekeys(upload) = proto_msg {
            let peer_hex = self.get_peer_id_for_session(envelope.session_id).await?;
            let key = format!("peer:{}:prekeys", peer_hex);

            let bundle = PrekeyBundle {
                identity_key: upload.identity_key,
                signed_prekey: upload.signed_prekey,
            };
            let value = serde_json::to_string(&bundle)?;

            let mut redis = self.redis.clone();
            redis
                .set_ex::<_, _, ()>(&key, &value, PREKEY_TTL_SECS)
                .await?;

            info!(peer = %peer_hex, "stored prekeys");
        }

        Ok(())
    }

    async fn handle_get_prekeys(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::KeysGetPrekeys(req) = proto_msg {
            let target_peer_hex = hex_encode(&req.peer_id);
            let key = format!("peer:{}:prekeys", target_peer_hex);

            let mut redis = self.redis.clone();
            let bundle_json: Option<String> = redis.get(&key).await?;

            let response = match bundle_json {
                Some(json) => {
                    let bundle: PrekeyBundle = serde_json::from_str(&json)?;
                    // Build a KeysGetPrekeys response as raw proto bytes
                    // so the gateway can forward it directly.
                    serde_json::json!({
                        "found": true,
                        "identity_key": bundle.identity_key,
                        "signed_prekey": bundle.signed_prekey,
                    })
                }
                None => {
                    serde_json::json!({
                        "found": false,
                        "error": "prekeys not found for peer",
                    })
                }
            };

            let reply_subject = format!("gateway.session.{}", envelope.session_id);
            self.nats
                .publish(reply_subject, Bytes::from(response.to_string()))
                .await?;

            debug!(target_peer = %target_peer_hex, "returned prekeys");
        }

        Ok(())
    }

    async fn handle_chat_send(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::ChatSend(mut chat) = proto_msg {
            let target_peer_hex = hex_encode(&chat.peer_id);

            // Replace peer_id with sender so the receiver knows who sent it.
            let sender_hex = self.get_peer_id_for_session(envelope.session_id).await?;
            let sender_bytes = hex_decode_bytes(&sender_hex);
            chat.peer_id = sender_bytes;

            let rewritten = chat.serialize();
            self.forward_to_peer(&target_peer_hex, &rewritten).await?;
            debug!(
                target_peer = %target_peer_hex,
                sender = %sender_hex,
                "forwarded chat message"
            );
        }

        Ok(())
    }

    /// Generic handler for file-transfer messages: deserialise the proto,
    /// extract the `peer_id` field, and forward the raw binary payload to that
    /// peer's gateway session.  The `msg_kind` parameter is only used for
    /// logging.
    async fn handle_file_forward(
        &self,
        msg: &async_nats::Message,
        msg_kind: &str,
    ) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let target_peer_hex = match dispatch(&envelope.payload)? {
            Message::FileOffer(m) => hex_encode(&m.peer_id),
            Message::FileAccept(m) => hex_encode(&m.peer_id),
            Message::FileChunk(m) => hex_encode(&m.peer_id),
            Message::FileComplete(m) => hex_encode(&m.peer_id),
            Message::FileChunkAck(m) => hex_encode(&m.peer_id),
            Message::FileResume(m) => hex_encode(&m.peer_id),
            other => {
                debug!(
                    kind = msg_kind,
                    "unexpected message type in handle_file_forward: {:?}", other
                );
                return Ok(());
            }
        };
        self.forward_to_peer(&target_peer_hex, &envelope.payload)
            .await?;
        debug!(target_peer = %target_peer_hex, kind = msg_kind, "forwarded file message");
        Ok(())
    }

    async fn handle_create_link(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        #[derive(Deserialize)]
        struct CreateLinkRequest {
            session_id: u64,
            peer_id: String,
        }

        let req: CreateLinkRequest = serde_json::from_slice(&msg.payload)?;
        let link_id = LinkId::generate();

        let link_key = format!("link:{}", link_id.as_str());
        let mut redis = self.redis.clone();
        redis
            .set_ex::<_, _, ()>(&link_key, &req.peer_id, LINK_TTL_SECS)
            .await?;

        // Send the link_id back to the creating peer.
        let response = serde_json::json!({
            "link_id": link_id.as_str(),
        });
        let reply_subject = format!("gateway.session.{}", req.session_id);
        self.nats
            .publish(reply_subject, Bytes::from(response.to_string()))
            .await?;

        LINKS_CREATED.inc();
        info!(link_id = %link_id.as_str(), peer = %req.peer_id, "created link");
        Ok(())
    }

    /// Forward a proto payload to a peer by looking up their gateway session in Redis.
    async fn forward_to_peer(&self, peer_id_hex: &str, payload: &[u8]) -> anyhow::Result<()> {
        let session_key = format!("peer:{}:session", peer_id_hex);
        let mut redis = self.redis.clone();
        let session_json: Option<String> = redis.get(&session_key).await?;

        match session_json {
            Some(json) => {
                let session: PeerSession = serde_json::from_str(&json)?;
                let target_subject = format!("gateway.session.{}", session.session_id);
                self.nats
                    .publish(target_subject, Bytes::from(payload.to_vec()))
                    .await?;
                Ok(())
            }
            None => {
                warn!(peer = %peer_id_hex, "peer session not found, cannot forward");
                Ok(())
            }
        }
    }

    /// Look up the peer_id (hex) for a given gateway session_id using the
    /// reverse index `session:{id}:peer` stored at registration time.
    async fn get_peer_id_for_session(&self, session_id: u64) -> anyhow::Result<String> {
        let mut redis = self.redis.clone();
        let reverse_key = format!("session:{}:peer", session_id);
        let peer_id: Option<String> = redis.get(&reverse_key).await?;
        peer_id.ok_or_else(|| anyhow::anyhow!("peer not found for session {}", session_id))
    }
}

/// Encode bytes as a hex string.
fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode_bytes(hex: &str) -> Vec<u8> {
    if !hex.len().is_multiple_of(2) {
        return Vec::new();
    }
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_common::init_tracing();
    let config = cypher_common::AppConfig::load()?;

    cypher_common::metrics::spawn_metrics_server(9091);

    info!("Signaling service starting");
    info!("  Redis: {}", config.redis_url);
    info!("  NATS:  {}", config.nats_url);
    info!("  STUN:  {}", config.stun_addr);

    // Parse the STUN bind address.
    let stun_addr: SocketAddr = config.stun_addr.parse()?;

    // Bind the STUN server and run it as a background task.
    let stun = StunServer::bind(stun_addr).await?;
    tokio::spawn(async move {
        stun.run().await;
    });

    let service = Arc::new(SignalingService::new(&config.redis_url, &config.nats_url).await?);

    info!("Signaling service running");
    service.run().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

    /// Deterministic transaction ID for tests.
    const TEST_TXN_ID: [u8; 12] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C,
    ];

    #[test]
    fn test_build_binding_response_ipv4() {
        let peer = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 100), 12345));
        let resp = build_binding_response(&TEST_TXN_ID, peer).expect("should produce response");

        // Parse using cypher-nat's parser to verify correctness.
        let addr = cypher_nat::parse_binding_response(&resp, &TEST_TXN_ID)
            .expect("should parse binding response");
        assert_eq!(addr, peer);
    }

    #[test]
    fn test_build_binding_response_ipv6() {
        let ip = Ipv6Addr::new(0x2001, 0x0db8, 0x85a3, 0, 0, 0x8a2e, 0x0370, 0x7334);
        let peer = SocketAddr::V6(SocketAddrV6::new(ip, 54321, 0, 0));
        let resp = build_binding_response(&TEST_TXN_ID, peer).expect("should produce response");

        // Parse using cypher-nat's parser which already supports IPv6.
        let addr = cypher_nat::parse_binding_response(&resp, &TEST_TXN_ID)
            .expect("should parse IPv6 binding response");
        assert_eq!(addr.ip(), peer.ip());
        assert_eq!(addr.port(), peer.port());
    }

    #[tokio::test]
    async fn test_stun_server_binding_roundtrip() {
        // Bind STUN server on an ephemeral port.
        let server = StunServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind STUN server");
        let server_addr = server.socket.local_addr().unwrap();

        // Run server in background.
        tokio::spawn(async move { server.run().await });

        // Use StunClient to send a binding request.
        let client = cypher_nat::StunClient::new()
            .await
            .expect("create STUN client");
        let reflexive = client
            .binding_request(server_addr)
            .await
            .expect("binding request");

        // Server is on localhost, so the reflexive address should be 127.0.0.1.
        assert_eq!(reflexive.ip(), Ipv4Addr::new(127, 0, 0, 1));
        // Port should be the client's local ephemeral port (non-zero).
        assert_ne!(reflexive.port(), 0);
    }
}
