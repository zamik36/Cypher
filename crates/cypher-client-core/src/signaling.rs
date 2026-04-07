//! High-level signaling client that speaks the `cypher_proto` binary protocol
//! toward the gateway (which routes messages via NATS to the signaling service).
//!
//! **Send direction:** serialised `cypher_proto` structs (constructor_id + fields).
//! **Receive direction:** JSON objects forwarded by the signaling service via
//! the gateway's `gateway.session.<id>` NATS subject.  Unknown binary payloads
//! are also returned to the caller so they can be dispatched as peer messages.

use bytes::Bytes;
use cypher_common::{Error, LinkId, PeerId, Result};
use cypher_proto::{Message, Serializable};
use cypher_transport::FrameFlags;
use tracing::debug;

use crate::connection::ServerConnection;

/// A single frame received from the server, discriminated at the transport
/// level before the caller dispatches it further.
#[derive(Debug)]
pub enum ServerFrame {
    /// A JSON envelope sent by the signaling service (response to a request or
    /// an asynchronous notification).
    Signaling(serde_json::Value),

    /// A raw proto binary payload forwarded from a remote peer via the relay /
    /// gateway.  The caller should pass this to `cypher_proto::dispatch`.
    Proto(Bytes),
}

/// Wraps a [`ServerConnection`] and provides typed send helpers for all
/// gateway-facing proto messages.
///
/// All methods that expect a response from the signaling service call
/// [`recv_server_frame`](SignalingClient::recv_server_frame) immediately after
/// sending, so requests are serialised by design.
pub struct SignalingClient {
    pub(crate) conn: ServerConnection,
}

impl SignalingClient {
    /// Create a [`SignalingClient`] from an already-connected
    /// [`ServerConnection`].
    pub fn new(conn: ServerConnection) -> Self {
        Self { conn }
    }

    // -----------------------------------------------------------------------
    // Receive helper
    // -----------------------------------------------------------------------

    /// Receive the next frame from the server and classify it.
    ///
    /// If the frame payload starts with `{` it is parsed as JSON (signaling
    /// service response / notification).  Otherwise it is treated as a raw
    /// proto binary payload from a peer.
    pub async fn recv_server_frame(&mut self) -> Result<ServerFrame> {
        let frame = self.conn.recv_frame().await?;
        let payload = frame.payload;

        if payload.first() == Some(&b'{') {
            let value: serde_json::Value = serde_json::from_slice(&payload)
                .map_err(|e| Error::Protocol(format!("malformed JSON from server: {e}")))?;
            Ok(ServerFrame::Signaling(value))
        } else {
            Ok(ServerFrame::Proto(payload))
        }
    }

    // -----------------------------------------------------------------------
    // Session
    // -----------------------------------------------------------------------

    /// Send a `SESSION_INIT` frame and wait for the gateway's `SESSION_ACK`.
    ///
    /// Returns the server nonce bytes from the ack.
    pub async fn session_init(&mut self, client_id: Vec<u8>, nonce: Vec<u8>) -> Result<Vec<u8>> {
        let msg = cypher_proto::SessionInit { client_id, nonce };
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::SESSION_INIT)
            .await?;
        debug!("sent SESSION_INIT");

        // The gateway replies synchronously with SESSION_ACK (same flag).
        let frame = self.conn.recv_frame().await?;
        let ack = cypher_proto::SessionAck::deserialize(&frame.payload)
            .map_err(|e| Error::Protocol(format!("invalid SESSION_ACK: {e}")))?;
        debug!(timestamp = ack.timestamp, "received SESSION_ACK");

        Ok(ack.server_nonce)
    }

    // -----------------------------------------------------------------------
    // Key exchange
    // -----------------------------------------------------------------------

    /// Upload our identity key and signed pre-key to the signaling service.
    ///
    /// Fire-and-forget: the server stores the keys in Redis with a session TTL.
    pub async fn upload_prekeys(
        &mut self,
        identity_key: Vec<u8>,
        signed_prekey: Vec<u8>,
        inbox_id: Vec<u8>,
    ) -> Result<()> {
        let msg = cypher_proto::KeysUploadPrekeys {
            identity_key,
            signed_prekey,
            inbox_id,
        };
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!("sent KeysUploadPrekeys");
        Ok(())
    }

    /// Fetch the key bundle for `peer_id` from the signaling service.
    ///
    /// Returns `(identity_key_bytes, signed_prekey_bytes)` on success.
    pub async fn get_peer_prekeys(&mut self, peer_id: &PeerId) -> Result<(Vec<u8>, Vec<u8>)> {
        let msg = cypher_proto::KeysGetPrekeys {
            peer_id: peer_id.to_vec(),
        };
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!(peer_id = %peer_id, "sent KeysGetPrekeys");

        let resp = self.expect_json_response("KeysGetPrekeys").await?;

        if resp.get("found").and_then(|v| v.as_bool()) != Some(true) {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("prekeys not found");
            return Err(Error::Protocol(err.to_string()));
        }

        let ik = json_bytes_field(&resp, "identity_key")?;
        let spk = json_bytes_field(&resp, "signed_prekey")?;
        Ok((ik, spk))
    }

    pub async fn get_transport_bootstrap(
        &mut self,
    ) -> Result<cypher_proto::TransportBootstrapInfo> {
        let msg = cypher_proto::TransportBootstrap {};
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!("sent TransportBootstrap");

        match self.recv_server_frame().await? {
            ServerFrame::Proto(payload) => match cypher_proto::dispatch(&payload)? {
                Message::TransportBootstrapInfo(info) => Ok(info),
                other => Err(Error::Protocol(format!(
                    "TransportBootstrap: unexpected proto response: {other:?}"
                ))),
            },
            ServerFrame::Signaling(_) => Err(Error::Protocol(
                "TransportBootstrap: expected proto response but got JSON".into(),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Link management
    // -----------------------------------------------------------------------

    /// Ask the signaling service to create a new share link.
    ///
    /// Returns the [`LinkId`] that can be shared with a peer.
    pub async fn create_link(&mut self) -> Result<LinkId> {
        // The signaling service's `create_link` handler reads a JSON envelope
        // (not a proto constructor), so we send JSON here.
        let envelope = serde_json::json!({ "action": "create_link" });
        self.conn
            .send_payload(Bytes::from(envelope.to_string()), FrameFlags::NONE)
            .await?;
        debug!("sent create_link");

        let resp = self.expect_json_response("create_link").await?;
        let id = resp
            .get("link_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Protocol("missing link_id in create_link response".into()))?;
        Ok(LinkId(id.to_string()))
    }

    /// Join an existing link by sending `signal.requestPeer`.
    ///
    /// Returns the remote peer's [`PeerId`].
    pub async fn join_link(&mut self, link_id: &LinkId) -> Result<PeerId> {
        let msg = cypher_proto::SignalRequestPeer {
            link_id: link_id.as_str().to_string(),
        };
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!(link_id = %link_id.as_str(), "sent SignalRequestPeer");

        let resp = self.expect_json_response("join_link").await?;

        if resp.get("found").and_then(|v| v.as_bool()) != Some(true) {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("link not found");
            return Err(Error::Protocol(err.to_string()));
        }

        let peer_id_hex = resp
            .get("peer_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Protocol("missing peer_id in join_link response".into()))?;

        let peer_id_bytes = hex_decode(peer_id_hex)?;
        PeerId::from_bytes(&peer_id_bytes)
            .ok_or_else(|| Error::Protocol("invalid peer_id bytes".into()))
    }

    // -----------------------------------------------------------------------
    // ICE candidate exchange
    // -----------------------------------------------------------------------

    /// Forward an ICE candidate string to a peer via the signaling service.
    pub async fn send_ice_candidate(&mut self, peer_id: &PeerId, candidate: &str) -> Result<()> {
        let msg = cypher_proto::SignalIceCandidate {
            candidate: candidate.to_string(),
            peer_id: peer_id.to_vec(),
        };
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!(peer_id = %peer_id, "sent ICE candidate");
        Ok(())
    }

    /// Forward an SDP offer to a peer.
    pub async fn send_offer(&mut self, peer_id: &PeerId, sdp: Vec<u8>) -> Result<()> {
        let msg = cypher_proto::SignalOffer {
            sdp,
            peer_id: peer_id.to_vec(),
        };
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!(peer_id = %peer_id, "sent SDP offer");
        Ok(())
    }

    /// Forward an SDP answer to a peer.
    pub async fn send_answer(&mut self, peer_id: &PeerId, sdp: Vec<u8>) -> Result<()> {
        let msg = cypher_proto::SignalAnswer {
            sdp,
            peer_id: peer_id.to_vec(),
        };
        self.conn
            .send_payload(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!(peer_id = %peer_id, "sent SDP answer");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    async fn expect_json_response(&mut self, context: &str) -> Result<serde_json::Value> {
        match self.recv_server_frame().await? {
            ServerFrame::Signaling(v) => Ok(v),
            ServerFrame::Proto(_) => Err(Error::Protocol(format!(
                "{context}: expected JSON response but got proto binary"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Extract a named field from a JSON object as `Vec<u8>`.
///
/// The field must be a JSON array of unsigned integers (as emitted by
/// `serde_json::json!` when serialising a `Vec<u8>` from the signaling service).
fn json_bytes_field(obj: &serde_json::Value, field: &str) -> Result<Vec<u8>> {
    let arr = obj
        .get(field)
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Protocol(format!("missing or non-array field '{field}'")))?;

    arr.iter()
        .map(|v| {
            v.as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| Error::Protocol(format!("invalid byte in '{field}'")))
        })
        .collect()
}

/// Decode a lowercase hex string into bytes.
fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(Error::Protocol("odd-length hex string".into()));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| Error::Protocol(format!("invalid hex at offset {i}")))
        })
        .collect()
}
