use bytes::Bytes;
use redis::AsyncCommands;
use serde::Deserialize;
use tracing::{debug, info};

use cypher_common::LinkId;
use cypher_proto::{dispatch, Message, Serializable};

use super::{
    hex_decode_bytes, hex_encode, GatewayEnvelope, PeerSession, PrekeyBundle, SignalingService,
    ICE_TTL_SECS, LINKS_CREATED, LINK_TTL_SECS, PEER_SESSIONS, PREKEY_TTL_SECS, SESSION_TTL_SECS,
};

impl SignalingService {
    pub(super) async fn handle_session_register(
        &self,
        msg: &async_nats::Message,
    ) -> anyhow::Result<()> {
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

        let reverse_key = format!("session:{}:peer", reg.session_id);
        redis
            .set_ex::<_, _, ()>(&reverse_key, &reg.peer_id, SESSION_TTL_SECS)
            .await?;

        PEER_SESSIONS.inc();
        info!(peer_id = %reg.peer_id, session_id = reg.session_id, "registered peer session");
        Ok(())
    }

    pub(super) async fn handle_request_peer(
        &self,
        msg: &async_nats::Message,
    ) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::SignalRequestPeer(req) = proto_msg {
            let link_key = format!("link:{}", req.link_id);
            let mut redis = self.redis.clone();
            let creator_peer_id: Option<String> = redis.get(&link_key).await?;

            match creator_peer_id {
                Some(peer_id_hex) => {
                    let session_key = format!("peer:{peer_id_hex}:session");
                    let session_json: Option<String> = redis.get(&session_key).await?;

                    let response = serde_json::json!({
                        "found": true,
                        "peer_id": peer_id_hex,
                        "session": session_json.and_then(|json| serde_json::from_str::<PeerSession>(&json).ok()),
                    });

                    let reply_subject = format!("gateway.session.{}", envelope.session_id);
                    self.nats
                        .publish(reply_subject, Bytes::from(response.to_string()))
                        .await?;

                    if let Ok(joiner_hex) = self.get_peer_id_for_session(envelope.session_id).await
                    {
                        let notification = serde_json::json!({
                            "peer_joined": true,
                            "peer_id": joiner_hex,
                        });
                        if let Err(error) = self
                            .forward_to_peer(&peer_id_hex, notification.to_string().as_bytes())
                            .await
                        {
                            debug!(err = %error, "could not notify link creator of peer join");
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

    pub(super) async fn handle_ice_candidate(
        &self,
        msg: &async_nats::Message,
    ) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::SignalIceCandidate(ice) = proto_msg {
            let target_peer_hex = hex_encode(&ice.peer_id);
            let source_peer_hex = self.get_peer_id_for_session(envelope.session_id).await?;

            let ice_key = format!("ice:{source_peer_hex}:{target_peer_hex}");
            let mut redis = self.redis.clone();
            let _: () = redis.rpush(&ice_key, &ice.candidate).await?;
            let _: () = redis.expire(&ice_key, ICE_TTL_SECS as i64).await?;

            self.forward_to_peer(&target_peer_hex, &envelope.payload)
                .await?;
            debug!(target_peer = %target_peer_hex, "forwarded ICE candidate");
        }

        Ok(())
    }

    pub(super) async fn handle_offer(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
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

    pub(super) async fn handle_answer(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
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

    pub(super) async fn handle_upload_prekeys(
        &self,
        msg: &async_nats::Message,
    ) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::KeysUploadPrekeys(upload) = proto_msg {
            let peer_hex = self.get_peer_id_for_session(envelope.session_id).await?;
            let key = format!("peer:{peer_hex}:prekeys");

            let inbox_id_hex = if upload.inbox_id.is_empty() {
                None
            } else {
                Some(hex_encode(&upload.inbox_id))
            };

            let bundle = PrekeyBundle {
                identity_key: upload.identity_key,
                signed_prekey: upload.signed_prekey,
                inbox_id: inbox_id_hex,
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

    pub(super) async fn handle_get_prekeys(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::KeysGetPrekeys(req) = proto_msg {
            let target_peer_hex = hex_encode(&req.peer_id);
            let key = format!("peer:{target_peer_hex}:prekeys");

            let mut redis = self.redis.clone();
            let bundle_json: Option<String> = redis.get(&key).await?;

            let response = match bundle_json {
                Some(json) => {
                    let bundle: PrekeyBundle = serde_json::from_str(&json)?;
                    serde_json::json!({
                        "found": true,
                        "identity_key": bundle.identity_key,
                        "signed_prekey": bundle.signed_prekey,
                        "inbox_id": bundle.inbox_id,
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

    pub(super) async fn handle_chat_send(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::ChatSend(mut chat) = proto_msg {
            let target_peer_hex = hex_encode(&chat.peer_id);
            let sender_hex = self.get_peer_id_for_session(envelope.session_id).await?;
            chat.peer_id = hex_decode_bytes(&sender_hex);

            let rewritten = chat.serialize();
            if self
                .try_forward_or_inbox(&target_peer_hex, &rewritten)
                .await?
            {
                debug!(target_peer = %target_peer_hex, sender = %sender_hex, "forwarded chat message");
            } else {
                debug!(target_peer = %target_peer_hex, sender = %sender_hex, "peer offline, stored in blind inbox");
            }
        }

        Ok(())
    }

    pub(super) async fn handle_file_forward(
        &self,
        msg: &async_nats::Message,
        msg_kind: &str,
    ) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let target_peer_hex = match dispatch(&envelope.payload)? {
            Message::FileOffer(message) => hex_encode(&message.peer_id),
            Message::FileAccept(message) => hex_encode(&message.peer_id),
            Message::FileChunk(message) => hex_encode(&message.peer_id),
            Message::FileComplete(message) => hex_encode(&message.peer_id),
            Message::FileChunkAck(message) => hex_encode(&message.peer_id),
            Message::FileResume(message) => hex_encode(&message.peer_id),
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

    pub(super) async fn handle_create_link(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
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

        let response = serde_json::json!({ "link_id": link_id.as_str() });
        let reply_subject = format!("gateway.session.{}", req.session_id);
        self.nats
            .publish(reply_subject, Bytes::from(response.to_string()))
            .await?;

        LINKS_CREATED.inc();
        info!(link_id = %link_id.as_str(), peer = %req.peer_id, "created link");
        Ok(())
    }
}
