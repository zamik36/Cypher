use bytes::Bytes;
use cypher_proto::{dispatch, Message, Serializable};
use redis::AsyncCommands;
use tracing::{debug, warn};

use super::{hex_decode_bytes, hex_encode, GatewayEnvelope, SignalingService};

const INBOX_TTL_SECS: i64 = 24 * 60 * 60;
const INBOX_MAX_MESSAGES: isize = 100;
const INBOX_CLAIM_TTL_SECS: i64 = 5 * 60;
const INBOX_CLAIM_INDEX_KEY: &str = "inbox:claims";
const INBOX_RECOVERY_INTERVAL_SECS: u64 = 5;

pub(super) async fn store_inbox_payload(
    redis: &mut redis::aio::ConnectionManager,
    inbox_hex: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let key = format!("inbox:{inbox_hex}");
    let encoded = hex_encode(payload);
    let _: () = redis.lpush(&key, &encoded).await?;
    let _: () = redis.ltrim(&key, 0, INBOX_MAX_MESSAGES - 1).await?;
    let _: () = redis.expire(&key, INBOX_TTL_SECS).await?;
    Ok(())
}

impl SignalingService {
    pub(super) async fn handle_inbox_store(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::InboxStore(store) = proto_msg {
            let inbox_hex = hex_encode(&store.inbox_id);
            let mut redis = self.redis.clone();
            store_inbox_payload(&mut redis, &inbox_hex, &store.ciphertext).await?;

            debug!(inbox = %inbox_hex, "stored inbox message");
        }

        Ok(())
    }

    pub(super) async fn handle_inbox_fetch(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::InboxFetch(fetch) = proto_msg {
            let inbox_hex = hex_encode(&fetch.inbox_id);
            let key = format!("inbox:{}", inbox_hex);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let claim_token = self.signer.generate_claim_token(&fetch.inbox_id, timestamp);
            let claim_key = format!("inbox:claim:{}", hex_encode(&claim_token));
            let claim_deadline = (timestamp + INBOX_CLAIM_TTL_SECS as u64) as i64;

            let mut redis = self.redis.clone();
            let messages: Vec<String> = redis::cmd("EVAL")
                .arg(
                    r#"
                    local msgs = redis.call('LRANGE', KEYS[1], 0, -1)
                    if #msgs > 0 then
                        local claim = cjson.encode({ inbox_id = ARGV[2], messages = msgs })
                        redis.call('SET', KEYS[2], claim)
                        redis.call('ZADD', KEYS[3], ARGV[1], KEYS[2])
                    end
                    redis.call('DEL', KEYS[1])
                    return msgs
                    "#,
                )
                .arg(3)
                .arg(&key)
                .arg(&claim_key)
                .arg(INBOX_CLAIM_INDEX_KEY)
                .arg(claim_deadline)
                .arg(&inbox_hex)
                .query_async(&mut redis)
                .await?;

            let mut blob = Vec::new();
            let count = messages.len() as u32;
            for encoded in &messages {
                let raw = hex_decode_bytes(encoded);
                if !raw.is_empty() {
                    blob.extend_from_slice(&(raw.len() as u32).to_le_bytes());
                    blob.extend_from_slice(&raw);
                }
            }

            let server_sig =
                self.signer
                    .sign_inbox_response(&blob, count, &fetch.inbox_id, timestamp);

            let response = cypher_proto::InboxMessages {
                messages: blob,
                count,
            };
            let mut payload = response.serialize();
            payload.extend_from_slice(&claim_token);
            payload.extend_from_slice(&server_sig);

            let reply_subject = msg
                .reply
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("gateway.session.{}", envelope.session_id));
            self.nats
                .publish(reply_subject, Bytes::from(payload))
                .await?;

            debug!(inbox = %inbox_hex, count, "fetched inbox messages (two-phase claim)");
        }

        Ok(())
    }

    pub(super) async fn handle_inbox_ack(&self, msg: &async_nats::Message) -> anyhow::Result<()> {
        let envelope: GatewayEnvelope = serde_json::from_slice(&msg.payload)?;
        let proto_msg = dispatch(&envelope.payload)?;

        if let Message::InboxAck(ack) = proto_msg {
            let inbox_hex = hex_encode(&ack.inbox_id);
            let claim_key = format!("inbox:claim:{}", hex_encode(&ack.claim_token));

            if !self.signer.verify_claim_token(
                &ack.inbox_id,
                &ack.claim_token,
                INBOX_CLAIM_TTL_SECS as u64,
            ) {
                warn!(inbox = %inbox_hex, "invalid claim token in InboxAck");
                return Ok(());
            }

            let mut redis = self.redis.clone();
            let _: i64 = redis::cmd("EVAL")
                .arg(
                    r#"
                    redis.call('DEL', KEYS[1])
                    redis.call('ZREM', KEYS[2], KEYS[1])
                    return 1
                    "#,
                )
                .arg(2)
                .arg(&claim_key)
                .arg(INBOX_CLAIM_INDEX_KEY)
                .query_async(&mut redis)
                .await?;
            debug!(inbox = %inbox_hex, "inbox claim acknowledged, backup deleted");

            let reply_subject = msg
                .reply
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("gateway.session.{}", envelope.session_id));
            let ok = cypher_proto::InboxMessages {
                messages: Vec::new(),
                count: 0,
            };
            self.nats
                .publish(reply_subject, Bytes::from(ok.serialize()))
                .await?;
        }

        Ok(())
    }

    pub(super) async fn recover_claims(self: std::sync::Arc<Self>) -> anyhow::Result<()> {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(INBOX_RECOVERY_INTERVAL_SECS)).await;

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let mut redis = self.redis.clone();
            let claim_keys: Vec<String> = redis::cmd("ZRANGEBYSCORE")
                .arg(INBOX_CLAIM_INDEX_KEY)
                .arg("-inf")
                .arg(now)
                .query_async(&mut redis)
                .await?;

            for claim_key in claim_keys {
                let restored: i64 = redis::cmd("EVAL")
                    .arg(
                        r#"
                        local claim = redis.call('GET', KEYS[1])
                        if not claim then
                            redis.call('ZREM', KEYS[2], KEYS[1])
                            return 0
                        end
                        local parsed = cjson.decode(claim)
                        local inbox_key = 'inbox:' .. parsed.inbox_id
                        for i = #parsed.messages, 1, -1 do
                            redis.call('LPUSH', inbox_key, parsed.messages[i])
                        end
                        redis.call('LTRIM', inbox_key, 0, tonumber(ARGV[1]) - 1)
                        redis.call('EXPIRE', inbox_key, ARGV[2])
                        redis.call('DEL', KEYS[1])
                        redis.call('ZREM', KEYS[2], KEYS[1])
                        return #parsed.messages
                        "#,
                    )
                    .arg(2)
                    .arg(&claim_key)
                    .arg(INBOX_CLAIM_INDEX_KEY)
                    .arg(INBOX_MAX_MESSAGES)
                    .arg(INBOX_TTL_SECS)
                    .query_async(&mut redis)
                    .await?;

                if restored > 0 {
                    debug!(claim_key = %claim_key, restored, "restored unacked inbox claim");
                }
            }
        }
    }
}
