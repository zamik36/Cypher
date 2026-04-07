use bytes::Bytes;
use cypher_proto::{dispatch, Message, Serializable};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use super::{GatewayEnvelope, SignalingService};

const CAPABILITY_SIGNED_INBOX: u32 = 1;
const CAPABILITY_RELAY: u32 = 1 << 1;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RelayBootstrapRecord {
    relay_addr: String,
    relay_public_key: Vec<u8>,
}

impl SignalingService {
    pub(super) async fn handle_transport_bootstrap(
        &self,
        msg: &async_nats::Message,
    ) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::TransportBootstrap(_) = proto_msg {
            let relay = self.load_relay_bootstrap().await?;
            let mut capabilities = CAPABILITY_SIGNED_INBOX;
            let (relay_addr, relay_public_key) = if let Some(relay) = relay {
                capabilities |= CAPABILITY_RELAY;
                (relay.relay_addr, relay.relay_public_key)
            } else {
                (String::new(), Vec::new())
            };

            let response = cypher_proto::TransportBootstrapInfo {
                relay_addr,
                relay_public_key,
                inbox_verifying_key: self.signer.verifying_key().to_bytes().to_vec(),
                capabilities,
            };
            let reply_subject = format!("gateway.session.{}", envelope.session_id);
            self.nats
                .publish(reply_subject, Bytes::from(response.serialize()))
                .await?;
        }

        Ok(())
    }

    async fn load_relay_bootstrap(&self) -> anyhow::Result<Option<RelayBootstrapRecord>> {
        let mut redis = self.redis.clone();
        let raw: Option<String> = redis.get("transport:relay:bootstrap").await?;
        raw.map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(Into::into)
    }
}
