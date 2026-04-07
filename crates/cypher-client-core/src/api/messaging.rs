use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use x25519_dalek::PublicKey as X25519PublicKey;

use cypher_common::{Error, PeerId, Result};
use cypher_proto::Serializable;
use cypher_transport::FrameFlags;

use crate::persistence::MessageStore;

use super::runtime::{hex_decode, json_bytes_field};
use super::{ClientApi, PendingKind};

impl ClientApi {
    /// Ask the server to create a new share link. Returns the link ID string.
    pub async fn create_link(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(PendingKind::CreateLink, tx);

        let envelope = serde_json::json!({ "action": "create_link" });
        self.send_raw(Bytes::from(envelope.to_string()), FrameFlags::NONE)
            .await?;

        let resp = rx
            .await
            .map_err(|_| Error::Session("create_link cancelled".into()))?;
        resp.get("link_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Protocol("missing link_id in response".into()))
    }

    /// Join a share link and return the remote peer's [`PeerId`].
    pub async fn join_link(&self, link: &str) -> Result<PeerId> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(PendingKind::JoinLink, tx);

        let msg = cypher_proto::SignalRequestPeer {
            link_id: link.to_string(),
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;

        let resp = rx
            .await
            .map_err(|_| Error::Session("join_link cancelled".into()))?;

        if resp.get("found").and_then(|v| v.as_bool()) != Some(true) {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("link not found");
            return Err(Error::Protocol(err.to_string()));
        }

        let hex = resp
            .get("peer_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Protocol("missing peer_id in response".into()))?;
        let bytes = hex_decode(hex)?;
        PeerId::from_bytes(&bytes).ok_or_else(|| Error::Protocol("invalid peer_id".into()))
    }

    /// Establish an E2EE session with `peer_id` as the X3DH initiator.
    ///
    /// Fetches the peer's prekeys from the signaling service, performs X3DH,
    /// and initialises the Double-Ratchet sender state.  After this call,
    /// [`send_message`](ClientApi::send_message) works for `peer_id`.
    pub async fn initiate_session(&self, peer_id: &PeerId) -> Result<()> {
        if self.keys.has_session(peer_id.as_bytes()) {
            debug!(peer_id = %peer_id, "session already exists, skipping initiate");
            return Ok(());
        }

        let (tx, rx) = oneshot::channel();
        self.pending.insert(PendingKind::GetPrekeys, tx);

        let msg = cypher_proto::KeysGetPrekeys {
            peer_id: peer_id.to_vec(),
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;

        let resp = rx
            .await
            .map_err(|_| Error::Session("get_prekeys cancelled".into()))?;

        if resp.get("found").and_then(|v| v.as_bool()) != Some(true) {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("prekeys not found");
            return Err(Error::Protocol(err.to_string()));
        }

        let ik_dh_bytes = json_bytes_field(&resp, "identity_key")?;
        let spk_bytes = json_bytes_field(&resp, "signed_prekey")?;
        let peer_inbox_id = resp.get("inbox_id").and_then(|v| {
            if v.is_null() {
                return None;
            }
            v.as_str().and_then(|hex| hex_decode(hex).ok())
        });

        let their_ik_dh = X25519PublicKey::from(
            <[u8; 32]>::try_from(ik_dh_bytes.as_slice())
                .map_err(|_| Error::Crypto("identity_key must be 32 bytes".into()))?,
        );
        let their_spk = X25519PublicKey::from(
            <[u8; 32]>::try_from(spk_bytes.as_slice())
                .map_err(|_| Error::Crypto("signed_prekey must be 32 bytes".into()))?,
        );

        let shared_secret = cypher_crypto::x3dh::x3dh_mutual(
            self.keys.identity(),
            &self.keys.spk_secret(),
            &their_ik_dh,
            &their_spk,
        );

        let our_id = self.session.peer_id().as_bytes();
        let their_id = peer_id.as_bytes();
        if our_id < their_id {
            self.keys
                .init_sender_session(peer_id.as_bytes(), &shared_secret, their_spk);
            info!(peer_id = %peer_id, role = "sender", "mutual key agreement session initialised");
        } else {
            self.keys
                .init_receiver_session(peer_id.as_bytes(), &shared_secret);
            info!(peer_id = %peer_id, role = "receiver", "mutual key agreement session initialised");
        }

        if let (Some(inbox), Some(store)) = (&peer_inbox_id, &self.message_store) {
            if let Err(e) = store.save_peer_inbox_id(peer_id, inbox) {
                warn!(peer_id = %peer_id, error = %e, "failed to persist peer inbox_id");
            }
        }

        Ok(())
    }

    /// Return a reference to the message store, if one was configured.
    pub fn message_store(&self) -> Option<&Arc<dyn MessageStore>> {
        self.message_store.as_ref()
    }

    /// Fetch queued offline messages from our blind inbox.
    pub async fn fetch_inbox(&self) -> Result<()> {
        if self.inbox_id.is_empty() {
            return Ok(());
        }
        let service = self
            .anonymous_service
            .lock()
            .await
            .clone()
            .ok_or_else(|| Error::Session("anonymous transport not initialized".into()))?;
        let results = service.fetch_all(vec![self.inbox_id.clone()]).await?;

        let outbound = self
            .outbound_tx
            .lock()
            .await
            .clone()
            .ok_or_else(|| Error::Session("not connected to gateway".into()))?;
        let runtime = self.runtime_context(outbound);

        for (_, payload) in results {
            runtime.dispatch_inbound(Bytes::from(payload)).await;
        }
        debug!("fetched inbox via anonymous transport");
        Ok(())
    }

    /// Store a message in a peer's blind inbox for offline delivery.
    pub async fn send_to_inbox(&self, inbox_id: &[u8], ciphertext: &[u8]) -> Result<()> {
        let msg = cypher_proto::InboxStore {
            inbox_id: inbox_id.to_vec(),
            ciphertext: ciphertext.to_vec(),
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!("sent InboxStore");
        Ok(())
    }

    /// Encrypt and send a message to `peer_id`.
    pub async fn send_message(&self, peer_id: &PeerId, plaintext: &[u8]) -> Result<()> {
        let (ciphertext, ratchet_key_bytes, msg_no) =
            self.keys.encrypt_for_peer(peer_id.as_bytes(), plaintext)?;

        let msg = cypher_proto::ChatSend {
            peer_id: peer_id.to_vec(),
            ciphertext,
            ratchet_key: ratchet_key_bytes,
            msg_no,
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!(peer_id = %peer_id, msg_no, "sent encrypted message");

        if let Some(store) = &self.message_store {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if let Err(e) =
                store.save_message(peer_id, crate::persistence::Direction::Sent, plaintext, now)
            {
                warn!(peer_id = %peer_id, error = %e, "failed to persist sent message; message was sent but may not appear in history after restart");
            }
            if let Some(state) = self.keys.get_ratchet_state(peer_id.as_bytes()) {
                if let Err(e) = store.save_ratchet_state(peer_id, &state) {
                    warn!(peer_id = %peer_id, error = %e, "failed to persist ratchet state; session may break after restart");
                }
            }
        }

        Ok(())
    }
}
