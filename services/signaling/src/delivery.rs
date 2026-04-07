use bytes::Bytes;
use redis::AsyncCommands;
use tracing::warn;

use super::inbox::store_inbox_payload;
use super::{PeerSession, PrekeyBundle, SignalingService};

impl SignalingService {
    /// Try to forward a message to a peer. If offline, store in their blind
    /// inbox (if they have one registered). Returns `true` if delivered online.
    pub(super) async fn try_forward_or_inbox(
        &self,
        peer_id_hex: &str,
        payload: &[u8],
    ) -> anyhow::Result<bool> {
        let session_key = format!("peer:{peer_id_hex}:session");
        let mut redis = self.redis.clone();
        let session_json: Option<String> = redis.get(&session_key).await?;

        if let Some(json) = session_json {
            let session: PeerSession = serde_json::from_str(&json)?;
            let target_subject = format!("gateway.session.{}", session.session_id);
            self.nats
                .publish(target_subject, Bytes::from(payload.to_vec()))
                .await?;
            return Ok(true);
        }

        let prekey_key = format!("peer:{peer_id_hex}:prekeys");
        let bundle_json: Option<String> = redis.get(&prekey_key).await?;
        if let Some(json) = bundle_json {
            if let Ok(bundle) = serde_json::from_str::<PrekeyBundle>(&json) {
                if let Some(inbox_hex) = &bundle.inbox_id {
                    store_inbox_payload(&mut redis, inbox_hex, payload).await?;
                    return Ok(false);
                }
            }
        }

        warn!(peer = %peer_id_hex, "peer offline, no inbox_id available; message dropped");
        Ok(false)
    }

    /// Forward a proto payload to a peer by looking up their gateway session in Redis.
    pub(super) async fn forward_to_peer(
        &self,
        peer_id_hex: &str,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        let session_key = format!("peer:{peer_id_hex}:session");
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

    /// Look up the peer_id (hex) for a given gateway session_id.
    pub(super) async fn get_peer_id_for_session(&self, session_id: u64) -> anyhow::Result<String> {
        let mut redis = self.redis.clone();
        let reverse_key = format!("session:{session_id}:peer");
        let peer_id: Option<String> = redis.get(&reverse_key).await?;
        peer_id.ok_or_else(|| anyhow::anyhow!("peer not found for session {session_id}"))
    }
}
