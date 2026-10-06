//! Redis persistence. Keys are short binary prefixes plus raw ids.

use bytes::Bytes;
use cypher_types::{LinkId, PeerId};
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Script};

const KEYS_TTL_SECS: u64 = 30 * 24 * 3600;
const LINK_TTL_SECS: u64 = 24 * 3600;
/// An inbox lives this long after its oldest unread item, and accepts
/// writes for this long after its owner last read it.
const INBOX_TTL_SECS: u64 = 14 * 24 * 3600;
const CLAIM_TTL_SECS: u64 = 120;
/// Bytes one inbox may hold, on top of [`MAX_INBOX_ITEMS`].
pub(crate) const MAX_INBOX_BYTES: usize = 8 << 20;
/// One-time prekeys kept per identity; a client never publishes more than
/// a batch at once, so anything beyond is abuse.
pub(crate) const MAX_STORED_OPKS: isize = 200;
/// Onion requests are remembered for twice the accepted clock skew, so any
/// replay still inside the timestamp window is recognised.
pub(crate) const REPLAY_TTL_SECS: u64 = 240;
pub(crate) const MAX_INBOX_ITEMS: usize = 1000;
/// Keeps every inbox batch below the NATS and frame payload limits.
pub(crate) const MAX_BATCH_BYTES: usize = 768 * 1024;

fn key(prefix: &[u8], id: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(prefix.len() + id.len());
    k.extend_from_slice(prefix);
    k.extend_from_slice(id);
    k
}

pub(crate) enum PutOutcome {
    Stored,
    Full,
    /// No owner has read this inbox within [`INBOX_TTL_SECS`]: it does not
    /// exist as far as writers are concerned.
    Inactive,
}

#[derive(Clone)]
pub(crate) struct Store {
    redis: ConnectionManager,
    put_script: Script,
    fetch_script: Script,
    ack_script: Script,
    admit_script: Script,
}

impl Store {
    pub(crate) async fn connect(url: &str) -> anyhow::Result<Self> {
        let redis = redis::Client::open(url)?.get_connection_manager().await?;
        Ok(Self {
            redis,
            // KEYS: items, byte count, activation. ARGV: item, max items,
            // max bytes, ttl. Neither key's expiry moves on a write, so a
            // flood cannot keep an inbox alive.
            put_script: Script::new(
                r"
                if redis.call('EXISTS', KEYS[3]) == 0 then return -1 end
                local bytes = tonumber(redis.call('GET', KEYS[2]) or '0')
                if redis.call('LLEN', KEYS[1]) >= tonumber(ARGV[2])
                  or bytes + #ARGV[1] > tonumber(ARGV[3]) then return 0 end
                redis.call('RPUSH', KEYS[1], ARGV[1])
                redis.call('EXPIRE', KEYS[1], ARGV[4], 'NX')
                redis.call('INCRBY', KEYS[2], #ARGV[1])
                redis.call('EXPIRE', KEYS[2], ARGV[4], 'NX')
                return 1",
            ),
            // KEYS: items, claim, activation. ARGV: claim hex, max items,
            // max bytes, claim ttl, inbox ttl. Reading (with the secret)
            // is what keeps an inbox open for writers.
            fetch_script: Script::new(
                r"
                redis.call('SET', KEYS[3], 1, 'EX', ARGV[5])
                if redis.call('EXISTS', KEYS[2]) == 1 then return {} end
                local items = redis.call('LRANGE', KEYS[1], 0, tonumber(ARGV[2]) - 1)
                local out, total = {}, 0
                for i, v in ipairs(items) do
                  if total + #v > tonumber(ARGV[3]) and i > 1 then break end
                  total = total + #v
                  out[#out + 1] = v
                end
                if #out > 0 then
                  redis.call('SET', KEYS[2], ARGV[1] .. ':' .. #out .. ':' .. total, 'EX', ARGV[4])
                end
                return out",
            ),
            // KEYS: items, claim, byte count. ARGV: claim hex.
            ack_script: Script::new(
                r"
                local v = redis.call('GET', KEYS[2])
                if not v or string.sub(v, 1, 32) ~= ARGV[1] then return 0 end
                local n, bytes = string.match(string.sub(v, 34), '^(%d+):(%d+)$')
                redis.call('LTRIM', KEYS[1], tonumber(n), -1)
                if redis.call('DECRBY', KEYS[3], tonumber(bytes)) <= 0 then
                  redis.call('DEL', KEYS[3])
                end
                redis.call('DEL', KEYS[2])
                return tonumber(n)",
            ),
            // A fixed-window counter. KEYS: bucket. ARGV: limit, window.
            admit_script: Script::new(
                r"
                local n = redis.call('INCR', KEYS[1])
                if n == 1 then redis.call('EXPIRE', KEYS[1], ARGV[2]) end
                if n > tonumber(ARGV[1]) then return 0 end
                return 1",
            ),
        })
    }

    pub(crate) async fn publish_keys(
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
            pipe.rpush(&opk_key, encoded)
                .ignore()
                .ltrim(&opk_key, -MAX_STORED_OPKS, -1)
                .ignore();
        }
        pipe.expire(&opk_key, i64::try_from(KEYS_TTL_SECS).unwrap_or(i64::MAX))
            .ignore()
            .llen(&opk_key);
        let (left,): (u64,) = pipe.query_async(&mut self.redis.clone()).await?;
        Ok(u16::try_from(left).unwrap_or(u16::MAX))
    }

    /// The bundle of `peer`, with one of its one-time prekeys unless
    /// `with_opk` is false.
    pub(crate) async fn fetch_keys(
        &self,
        peer: &PeerId,
        with_opk: bool,
    ) -> redis::RedisResult<Option<(Bytes, Option<(u32, [u8; 32])>)>> {
        let mut redis = self.redis.clone();
        let base_key = key(b"k:", peer.as_bytes());
        let (base, opk): (Option<Vec<u8>>, Option<Vec<u8>>) = if with_opk {
            redis::pipe()
                .get(base_key)
                .lpop(key(b"o:", peer.as_bytes()), None)
                .query_async(&mut redis)
                .await?
        } else {
            (redis.get(base_key).await?, None)
        };
        let opk = opk.and_then(|e| {
            let id = u32::from_le_bytes(e.get(..4)?.try_into().ok()?);
            Some((id, e.get(4..36)?.try_into().ok()?))
        });
        Ok(base.map(|b| (Bytes::from(b), opk)))
    }

    pub(crate) async fn create_link(
        &self,
        link: &LinkId,
        peer: &PeerId,
    ) -> redis::RedisResult<bool> {
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

    /// Remembers an onion request; `false` when it was already seen.
    pub(crate) async fn first_seen(&self, replay_id: &[u8; 32]) -> redis::RedisResult<bool> {
        let mut redis = self.redis.clone();
        redis::cmd("SET")
            .arg(key(b"r:", replay_id))
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(REPLAY_TTL_SECS)
            .query_async::<Option<String>>(&mut redis)
            .await
            .map(|r| r.is_some())
    }

    pub(crate) async fn resolve_link(&self, link: &LinkId) -> redis::RedisResult<Option<PeerId>> {
        let raw: Option<Vec<u8>> = self
            .redis
            .clone()
            .get(key(b"l:", link.as_str().as_bytes()))
            .await?;
        Ok(raw.and_then(|r| PeerId::from_bytes(&r)))
    }

    /// Seconds left before `prefix ‖ id` expires.
    #[cfg(test)]
    pub(crate) async fn ttl(&self, prefix: &[u8], id: &[u8]) -> i64 {
        self.redis.clone().ttl(key(prefix, id)).await.unwrap()
    }

    /// Counts one event in `bucket`; `false` once more than `limit` fell
    /// into the current `window_secs`.
    pub(crate) async fn admit(
        &self,
        bucket: &[u8],
        limit: u32,
        window_secs: u64,
    ) -> redis::RedisResult<bool> {
        let admitted: i64 = self
            .admit_script
            .key(key(b"q:", bucket))
            .arg(limit)
            .arg(window_secs)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(admitted == 1)
    }

    /// Keeps the push subscription of `inbox` as long as the inbox lives.
    pub(crate) async fn push_register(
        &self,
        inbox: &[u8; 32],
        sub: &[u8],
    ) -> redis::RedisResult<()> {
        let mut redis = self.redis.clone();
        redis::cmd("SET")
            .arg(key(b"p:", inbox))
            .arg(sub)
            .arg("EX")
            .arg(INBOX_TTL_SECS)
            .query_async::<()>(&mut redis)
            .await
    }

    pub(crate) async fn push_subscription(
        &self,
        inbox: &[u8; 32],
    ) -> redis::RedisResult<Option<Vec<u8>>> {
        self.redis.clone().get(key(b"p:", inbox)).await
    }

    pub(crate) async fn push_unregister(&self, inbox: &[u8; 32]) -> redis::RedisResult<()> {
        self.redis.clone().del(key(b"p:", inbox)).await
    }

    pub(crate) async fn inbox_put(
        &self,
        inbox: &[u8; 32],
        item: &[u8],
    ) -> redis::RedisResult<PutOutcome> {
        let stored: i64 = self
            .put_script
            .key(key(b"i:", inbox))
            .key(key(b"b:", inbox))
            .key(key(b"a:", inbox))
            .arg(item)
            .arg(MAX_INBOX_ITEMS)
            .arg(MAX_INBOX_BYTES)
            .arg(INBOX_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(match stored {
            1 => PutOutcome::Stored,
            -1 => PutOutcome::Inactive,
            _ => PutOutcome::Full,
        })
    }

    /// Claims the oldest items exclusively until acked or the claim expires.
    pub(crate) async fn inbox_fetch(
        &self,
        inbox: &[u8; 32],
        claim: &[u8; 16],
        max_items: usize,
    ) -> redis::RedisResult<Vec<Bytes>> {
        let items: Vec<Vec<u8>> = self
            .fetch_script
            .key(key(b"i:", inbox))
            .key(key(b"c:", inbox))
            .key(key(b"a:", inbox))
            .arg(hex::encode(claim))
            .arg(max_items)
            .arg(MAX_BATCH_BYTES)
            .arg(CLAIM_TTL_SECS)
            .arg(INBOX_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(items.into_iter().map(Bytes::from).collect())
    }

    pub(crate) async fn inbox_ack(
        &self,
        inbox: &[u8; 32],
        claim: &[u8; 16],
    ) -> redis::RedisResult<()> {
        let _: i64 = self
            .ack_script
            .key(key(b"i:", inbox))
            .key(key(b"c:", inbox))
            .key(key(b"b:", inbox))
            .arg(hex::encode(claim))
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(())
    }
}
