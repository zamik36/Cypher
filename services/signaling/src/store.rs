//! Redis persistence. Keys are short binary prefixes plus raw ids.

use bytes::Bytes;
use cypher_types::{LinkId, PeerId};
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Script};

const KEYS_TTL_SECS: u64 = 30 * 24 * 3600;
const LINK_TTL_SECS: u64 = 24 * 3600;
const INBOX_TTL_SECS: u64 = 14 * 24 * 3600;
const CLAIM_TTL_SECS: u64 = 120;
pub const MAX_INBOX_ITEMS: usize = 1000;
/// Keeps every inbox batch below the NATS and frame payload limits.
pub const MAX_BATCH_BYTES: usize = 768 * 1024;

fn key(prefix: &[u8], id: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(prefix.len() + id.len());
    k.extend_from_slice(prefix);
    k.extend_from_slice(id);
    k
}

pub enum PutOutcome {
    Stored,
    Full,
}

#[derive(Clone)]
pub struct Store {
    redis: ConnectionManager,
    put_script: Script,
    fetch_script: Script,
    ack_script: Script,
}

impl Store {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let redis = redis::Client::open(url)?.get_connection_manager().await?;
        Ok(Self {
            redis,
            put_script: Script::new(
                r"
                if redis.call('LLEN', KEYS[1]) >= tonumber(ARGV[2]) then return 0 end
                redis.call('RPUSH', KEYS[1], ARGV[1])
                redis.call('EXPIRE', KEYS[1], ARGV[3])
                return 1",
            ),
            fetch_script: Script::new(
                r"
                if redis.call('EXISTS', KEYS[2]) == 1 then return {} end
                local items = redis.call('LRANGE', KEYS[1], 0, tonumber(ARGV[2]) - 1)
                local out, total = {}, 0
                for i, v in ipairs(items) do
                  total = total + #v
                  if total > tonumber(ARGV[3]) and i > 1 then break end
                  out[#out + 1] = v
                end
                if #out > 0 then
                  redis.call('SET', KEYS[2], ARGV[1] .. #out, 'EX', ARGV[4])
                end
                return out",
            ),
            ack_script: Script::new(
                r"
                local v = redis.call('GET', KEYS[2])
                if not v or string.sub(v, 1, 32) ~= ARGV[1] then return 0 end
                local n = tonumber(string.sub(v, 33))
                redis.call('LTRIM', KEYS[1], n, -1)
                redis.call('DEL', KEYS[2])
                return n",
            ),
        })
    }

    pub async fn publish_keys(
        &self,
        peer: &PeerId,
        base: &[u8],
        opks: &[(u32, [u8; 32])],
        replace: bool,
    ) -> redis::RedisResult<u16> {
        let opk_key = key(b"o:", peer.as_bytes());
        let mut pipe = redis::pipe();
        pipe.atomic()
            .set_ex(key(b"k:", peer.as_bytes()), base, KEYS_TTL_SECS)
            .ignore();
        if replace {
            pipe.del(&opk_key).ignore();
        }
        if !opks.is_empty() {
            let encoded: Vec<Vec<u8>> = opks
                .iter()
                .map(|(id, k)| {
                    let mut e = id.to_le_bytes().to_vec();
                    e.extend_from_slice(k);
                    e
                })
                .collect();
            pipe.rpush(&opk_key, encoded).ignore();
        }
        pipe.expire(&opk_key, KEYS_TTL_SECS as i64)
            .ignore()
            .llen(&opk_key);
        let (left,): (u64,) = pipe.query_async(&mut self.redis.clone()).await?;
        Ok(u16::try_from(left).unwrap_or(u16::MAX))
    }

    pub async fn fetch_keys(
        &self,
        peer: &PeerId,
    ) -> redis::RedisResult<Option<(Bytes, Option<(u32, [u8; 32])>)>> {
        let mut redis = self.redis.clone();
        let (base, opk): (Option<Vec<u8>>, Option<Vec<u8>>) = redis::pipe()
            .get(key(b"k:", peer.as_bytes()))
            .lpop(key(b"o:", peer.as_bytes()), None)
            .query_async(&mut redis)
            .await?;
        let opk = opk.and_then(|e| {
            let id = u32::from_le_bytes(e.get(..4)?.try_into().ok()?);
            Some((id, e.get(4..36)?.try_into().ok()?))
        });
        Ok(base.map(|b| (Bytes::from(b), opk)))
    }

    pub async fn create_link(&self, link: &LinkId, peer: &PeerId) -> redis::RedisResult<bool> {
        let mut redis = self.redis.clone();
        redis::cmd("SET")
            .arg(key(b"l:", link.as_str().as_bytes()))
            .arg(peer.as_bytes())
            .arg("NX")
            .arg("EX")
            .arg(LINK_TTL_SECS)
            .query_async::<Option<String>>(&mut redis)
            .await
            .map(|r| r.is_some())
    }

    pub async fn resolve_link(&self, link: &LinkId) -> redis::RedisResult<Option<PeerId>> {
        let raw: Option<Vec<u8>> = self
            .redis
            .clone()
            .get(key(b"l:", link.as_str().as_bytes()))
            .await?;
        Ok(raw.and_then(|r| PeerId::from_bytes(&r)))
    }

    pub async fn inbox_put(&self, inbox: &[u8; 32], item: &[u8]) -> redis::RedisResult<PutOutcome> {
        let stored: i64 = self
            .put_script
            .key(key(b"i:", inbox))
            .arg(item)
            .arg(MAX_INBOX_ITEMS)
            .arg(INBOX_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(if stored == 1 {
            PutOutcome::Stored
        } else {
            PutOutcome::Full
        })
    }

    /// Claims the oldest items exclusively until acked or the claim expires.
    pub async fn inbox_fetch(
        &self,
        inbox: &[u8; 32],
        claim: &[u8; 16],
        max_items: usize,
    ) -> redis::RedisResult<Vec<Bytes>> {
        let items: Vec<Vec<u8>> = self
            .fetch_script
            .key(key(b"i:", inbox))
            .key(key(b"c:", inbox))
            .arg(hex::encode(claim))
            .arg(max_items)
            .arg(MAX_BATCH_BYTES)
            .arg(CLAIM_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(items.into_iter().map(Bytes::from).collect())
    }

    pub async fn inbox_ack(&self, inbox: &[u8; 32], claim: &[u8; 16]) -> redis::RedisResult<()> {
        let _: i64 = self
            .ack_script
            .key(key(b"i:", inbox))
            .key(key(b"c:", inbox))
            .arg(hex::encode(claim))
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(())
    }
}
