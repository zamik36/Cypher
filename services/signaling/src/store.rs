//! Redis persistence. Keys are short binary prefixes plus raw ids.

use bytes::Bytes;
use cypher_types::{Addr, DeviceId, LinkId, PeerId};
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
/// Bytes all inboxes together may hold: half of Redis' `maxmemory` in the
/// standard deploy (`REDIS_MAXMEMORY`, 512 MiB, in `docker-compose.yml`).
/// Redis refuses every write once full (`noeviction`), and prekeys, device
/// lists, onion replay ids and rate limits live there too, so anonymous
/// inbox items must never fill it. Raise it along with `maxmemory`.
pub(crate) const INBOX_BUDGET_BYTES: u64 = 256 << 20;
/// What one inbox item costs Redis beyond its bytes (list entry, node
/// headers), charged against [`INBOX_BUDGET_BYTES`] so that a flood of
/// tiny items cannot hold far more memory than the budget says.
pub(crate) const INBOX_ITEM_OVERHEAD: u64 = 64;
/// Inboxes owners may open per inbox epoch (a week), all together. A
/// device opens about one new inbox a week (the next epoch's), so this is
/// room for some 100 000 devices; it bounds how many inboxes anonymous
/// key-making can open, each a few hundred bytes of Redis even when empty.
pub(crate) const MAX_INBOX_ACTIVATIONS_PER_EPOCH: u64 = 100_000;
const DAY_SECS: u64 = 24 * 3600;
/// Inbox bytes are counted per day on which they expire, today and the
/// days an inbox can live on: the counters expire with what they count,
/// so they cannot drift however items leave.
const BUDGET_DAYS: u64 = INBOX_TTL_SECS / DAY_SECS + 1;

/// Lua for the inbox scripts: the day counter the items of list KEYS[1]
/// count on, and when that counter expires. The day counters are the keys
/// from `first` on, one per day from today.
const BUDGET_BUCKET: &str = r"
    local function budget_bucket(first, now)
      local today = math.floor(now / 86400)
      local left = math.max(redis.call('TTL', KEYS[1]), 0)
      local day = math.floor((now + left) / 86400) - today
      day = math.max(0, math.min(day, #KEYS - first))
      return KEYS[first + day], (today + day + 1) * 86400
    end
";

/// KEYS: items, byte count, activation, day counters. ARGV: item, max
/// items, max bytes, ttl, budget, now, item overhead. Neither key's expiry
/// moves on a write, so a flood cannot keep an inbox alive.
const PUT_SCRIPT: &str = r"
    if redis.call('EXISTS', KEYS[3]) == 0 then return -1 end
    local size = #ARGV[1]
    local bytes = tonumber(redis.call('GET', KEYS[2]) or '0')
    if redis.call('LLEN', KEYS[1]) >= tonumber(ARGV[2])
      or bytes + size > tonumber(ARGV[3]) then return 0 end
    local cost, held = size + tonumber(ARGV[7]), 0
    for _, v in ipairs(redis.call('MGET', unpack(KEYS, 4))) do
      held = held + math.max(tonumber(v or '0'), 0)
    end
    if held + cost > tonumber(ARGV[5]) then return -2 end
    redis.call('RPUSH', KEYS[1], ARGV[1])
    redis.call('EXPIRE', KEYS[1], ARGV[4], 'NX')
    redis.call('INCRBY', KEYS[2], size)
    redis.call('EXPIRE', KEYS[2], ARGV[4], 'NX')
    local bucket, ends = budget_bucket(4, tonumber(ARGV[6]))
    redis.call('INCRBY', bucket, cost)
    redis.call('EXPIREAT', bucket, ends)
    return 1";

/// KEYS: items, claim, byte count, day counters. ARGV: claim hex, now,
/// item overhead. A day counter is lowered only while it lives: one that
/// expired counts nothing any more.
const ACK_SCRIPT: &str = r"
    local v = redis.call('GET', KEYS[2])
    if not v or string.sub(v, 1, 32) ~= ARGV[1] then return 0 end
    local n, bytes = string.match(string.sub(v, 34), '^(%d+):(%d+)$')
    n, bytes = tonumber(n), tonumber(bytes)
    local bucket = budget_bucket(4, tonumber(ARGV[2]))
    local listed = redis.call('EXISTS', KEYS[1]) == 1
    redis.call('LTRIM', KEYS[1], n, -1)
    if redis.call('DECRBY', KEYS[3], bytes) <= 0 then
      redis.call('DEL', KEYS[3])
    end
    redis.call('DEL', KEYS[2])
    if listed and redis.call('EXISTS', bucket) == 1
      and redis.call('DECRBY', bucket, bytes + n * tonumber(ARGV[3])) <= 0 then
      redis.call('DEL', bucket)
    end
    return n";

/// KEYS: items, claim, activation, push, inboxes opened this epoch. ARGV:
/// claim hex, max items, max bytes, claim ttl, inbox ttl, close, max
/// openings, openings ttl. Reading (as the owner) is what keeps an inbox
/// open for writers; closing shuts it to them and drops its push
/// subscription at once. Opening an inbox that is not open counts against
/// the epoch's openings; past them the answer is nil and nothing changes.
const FETCH_SCRIPT: &str = r"
    if ARGV[6] == '1' then
      redis.call('DEL', KEYS[3], KEYS[4])
    elseif redis.call('EXISTS', KEYS[3]) == 1 then
      redis.call('SET', KEYS[3], 1, 'EX', ARGV[5])
    else
      if tonumber(redis.call('GET', KEYS[5]) or '0') >= tonumber(ARGV[7]) then
        return false
      end
      redis.call('INCR', KEYS[5])
      redis.call('EXPIRE', KEYS[5], ARGV[8], 'NX')
      redis.call('SET', KEYS[3], 1, 'EX', ARGV[5])
    end
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
    return out";

fn key(prefix: &[u8], id: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(prefix.len() + id.len());
    k.extend_from_slice(prefix);
    k.extend_from_slice(id);
    k
}

/// Key of something one device of a peer owns: `prefix ‖ peer ‖ device`.
fn device_key(prefix: &[u8], addr: Addr) -> Vec<u8> {
    let mut k = key(prefix, addr.peer.as_bytes());
    k.extend_from_slice(&addr.device.0.to_le_bytes());
    k
}

/// KEYS: list, version. ARGV: version (fixed-width decimal, so string order
/// is number order), list, ttl. Only a newer version replaces the stored one.
const DEVICES_SCRIPT: &str = r"
    local stored = redis.call('GET', KEYS[2])
    if stored and stored >= ARGV[1] then return 0 end
    redis.call('SET', KEYS[1], ARGV[2], 'EX', ARGV[3])
    redis.call('SET', KEYS[2], ARGV[1], 'EX', ARGV[3])
    return 1";

/// KEYS: unlisted devices. ARGV: device, max devices, ttl. Whether the
/// device may hold keys while its identity's list does not name it: it
/// already does, or there is room for one more such device.
const UNLISTED_SCRIPT: &str = r"
    if redis.call('SISMEMBER', KEYS[1], ARGV[1]) == 1 then return 1 end
    if redis.call('SCARD', KEYS[1]) >= tonumber(ARGV[2]) then return 0 end
    redis.call('SADD', KEYS[1], ARGV[1])
    redis.call('EXPIRE', KEYS[1], ARGV[3])
    return 1";

pub(crate) enum PutOutcome {
    Stored,
    Full,
    /// All inboxes together hold their budget already.
    OverBudget,
    /// No owner has read this inbox within [`INBOX_TTL_SECS`]: it does not
    /// exist as far as writers are concerned.
    Inactive,
}

/// What all inboxes together may take.
#[derive(Debug, Clone)]
pub(crate) struct InboxQuota {
    /// Bytes, see [`INBOX_BUDGET_BYTES`].
    pub budget: u64,
    /// Inboxes opened per epoch, see [`MAX_INBOX_ACTIVATIONS_PER_EPOCH`].
    pub activations: u64,
    /// Prefix of the shared counters' keys, so tests keep their own.
    pub scope: Vec<u8>,
}

impl Default for InboxQuota {
    fn default() -> Self {
        Self {
            budget: INBOX_BUDGET_BYTES,
            activations: MAX_INBOX_ACTIVATIONS_PER_EPOCH,
            scope: Vec::new(),
        }
    }
}

impl InboxQuota {
    /// The day counters of inbox bytes, from the day `now` falls on.
    fn day_counters(&self, now: u64) -> Vec<Vec<u8>> {
        let today = now / DAY_SECS;
        (today..today.saturating_add(BUDGET_DAYS))
            .map(|day| [self.scope.as_slice(), b"ib:", &day.to_le_bytes()].concat())
            .collect()
    }

    /// The counter of inboxes opened in the epoch `now` falls on.
    fn activations_counter(&self, now: u64) -> Vec<u8> {
        let epoch = cypher_crypto::inbox::inbox_epoch(now);
        [self.scope.as_slice(), b"ia:", &epoch.to_le_bytes()].concat()
    }
}

#[derive(Clone)]
pub(crate) struct Store {
    redis: ConnectionManager,
    quota: InboxQuota,
    put_script: Script,
    fetch_script: Script,
    ack_script: Script,
    admit_script: Script,
    devices_script: Script,
    unlisted_script: Script,
}

impl Store {
    pub(crate) async fn connect(url: &str) -> anyhow::Result<Self> {
        Self::connect_with(url, InboxQuota::default()).await
    }

    pub(crate) async fn connect_with(url: &str, quota: InboxQuota) -> anyhow::Result<Self> {
        let redis = redis::Client::open(url)?.get_connection_manager().await?;
        Ok(Self {
            redis,
            quota,
            put_script: Script::new(&[BUDGET_BUCKET, PUT_SCRIPT].concat()),
            fetch_script: Script::new(FETCH_SCRIPT),
            ack_script: Script::new(&[BUDGET_BUCKET, ACK_SCRIPT].concat()),
            // A fixed-window counter. KEYS: bucket. ARGV: limit, window.
            admit_script: Script::new(
                r"
                local n = redis.call('INCR', KEYS[1])
                if n == 1 then redis.call('EXPIRE', KEYS[1], ARGV[2]) end
                if n > tonumber(ARGV[1]) then return 0 end
                return 1",
            ),
            devices_script: Script::new(DEVICES_SCRIPT),
            unlisted_script: Script::new(UNLISTED_SCRIPT),
        })
    }

    /// Stores one device's bundle and one-time prekeys.
    pub(crate) async fn publish_keys(
        &self,
        addr: Addr,
        base: &[u8],
        opks: &[(u32, [u8; 32])],
        replace: bool,
    ) -> redis::RedisResult<u16> {
        let opk_key = device_key(b"o:", addr);
        let mut pipe = redis::pipe();
        pipe.atomic()
            .set_ex(device_key(b"k:", addr), base, KEYS_TTL_SECS)
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
        // A device that keeps publishing keeps its identity's device list
        // alive too.
        let ttl = i64::try_from(KEYS_TTL_SECS).unwrap_or(i64::MAX);
        pipe.expire(key(b"d:", addr.peer.as_bytes()), ttl)
            .ignore()
            .expire(key(b"dv:", addr.peer.as_bytes()), ttl)
            .ignore()
            .expire(&opk_key, ttl)
            .ignore()
            .llen(&opk_key);
        let (left,): (u64,) = pipe.query_async(&mut self.redis.clone()).await?;
        Ok(u16::try_from(left).unwrap_or(u16::MAX))
    }

    /// The bundle of one device of a peer, with one of its one-time
    /// prekeys unless `with_opk` is false.
    pub(crate) async fn fetch_keys(
        &self,
        addr: Addr,
        with_opk: bool,
    ) -> redis::RedisResult<Option<(Bytes, Option<(u32, [u8; 32])>)>> {
        let mut redis = self.redis.clone();
        let base_key = device_key(b"k:", addr);
        let (base, opk): (Option<Vec<u8>>, Option<Vec<u8>>) = if with_opk {
            redis::pipe()
                .get(base_key)
                .lpop(device_key(b"o:", addr), None)
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

    /// Stores `peer`'s signed device list unless one at `version` or newer
    /// is stored already; `false` then.
    pub(crate) async fn publish_devices(
        &self,
        peer: &PeerId,
        version: u64,
        list: &[u8],
    ) -> redis::RedisResult<bool> {
        let stored: i64 = self
            .devices_script
            .key(key(b"d:", peer.as_bytes()))
            .key(key(b"dv:", peer.as_bytes()))
            .arg(format!("{version:020}"))
            .arg(list)
            .arg(KEYS_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(stored == 1)
    }

    /// `peer`'s signed device list, as published.
    pub(crate) async fn fetch_devices(&self, peer: &PeerId) -> redis::RedisResult<Option<Bytes>> {
        let list: Option<Vec<u8>> = self.redis.clone().get(key(b"d:", peer.as_bytes())).await?;
        Ok(list.map(Bytes::from))
    }

    /// Notes that `addr` holds keys though its identity's list does not
    /// name it; `false` when `max` such devices hold keys already.
    pub(crate) async fn admit_unlisted(&self, addr: Addr, max: usize) -> redis::RedisResult<bool> {
        let admitted: i64 = self
            .unlisted_script
            .key(key(b"u:", addr.peer.as_bytes()))
            .arg(addr.device.0)
            .arg(max)
            .arg(KEYS_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(admitted == 1)
    }

    /// The devices of `peer` that hold keys though its list did not name
    /// them, forgotten as such.
    pub(crate) async fn take_unlisted(&self, peer: &PeerId) -> redis::RedisResult<Vec<DeviceId>> {
        let unlisted = key(b"u:", peer.as_bytes());
        let (devices,): (Vec<u32>,) = redis::pipe()
            .atomic()
            .smembers(&unlisted)
            .del(&unlisted)
            .ignore()
            .query_async(&mut self.redis.clone())
            .await?;
        Ok(devices.into_iter().map(DeviceId).collect())
    }

    /// Forgets the keys of a device its identity no longer lists.
    pub(crate) async fn forget_device_keys(&self, addr: Addr) -> redis::RedisResult<()> {
        let keys = [device_key(b"k:", addr), device_key(b"o:", addr)];
        self.redis.clone().del(&keys).await
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

    /// An epoch's onion key as stored, wrapped, under `name` (see
    /// [`crate::keys`]).
    pub(crate) async fn onion_key(&self, name: &[u8]) -> redis::RedisResult<Option<Vec<u8>>> {
        let mut redis = self.redis.clone();
        redis.get(key(b"x:", name)).await
    }

    /// Stores an epoch's wrapped onion key for `ttl_secs`, unless another
    /// instance stored one first.
    pub(crate) async fn put_onion_key(
        &self,
        name: &[u8],
        wrapped: &[u8],
        ttl_secs: u64,
    ) -> redis::RedisResult<()> {
        let mut redis = self.redis.clone();
        redis::cmd("SET")
            .arg(key(b"x:", name))
            .arg(wrapped)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async::<Option<String>>(&mut redis)
            .await
            .map(drop)
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
        let now = crate::now_secs();
        let stored: i64 = self
            .put_script
            .key(key(b"i:", inbox))
            .key(key(b"b:", inbox))
            .key(key(b"a:", inbox))
            .key(self.quota.day_counters(now))
            .arg(item)
            .arg(MAX_INBOX_ITEMS)
            .arg(MAX_INBOX_BYTES)
            .arg(INBOX_TTL_SECS)
            .arg(self.quota.budget)
            .arg(now)
            .arg(INBOX_ITEM_OVERHEAD)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(match stored {
            1 => PutOutcome::Stored,
            -1 => PutOutcome::Inactive,
            -2 => PutOutcome::OverBudget,
            _ => PutOutcome::Full,
        })
    }

    /// Claims the oldest items exclusively until acked or the claim expires.
    /// With `close`, writers find the inbox gone from now on; what it still
    /// holds stays readable until it expires. `None` when the inbox is not
    /// open and no more inboxes may be opened this epoch.
    pub(crate) async fn inbox_fetch(
        &self,
        inbox: &[u8; 32],
        claim: &[u8; 16],
        max_items: usize,
        close: bool,
    ) -> redis::RedisResult<Option<Vec<Bytes>>> {
        let now = crate::now_secs();
        let items: Option<Vec<Vec<u8>>> = self
            .fetch_script
            .key(key(b"i:", inbox))
            .key(key(b"c:", inbox))
            .key(key(b"a:", inbox))
            .key(key(b"p:", inbox))
            .key(self.quota.activations_counter(now))
            .arg(hex::encode(claim))
            .arg(max_items)
            .arg(MAX_BATCH_BYTES)
            .arg(CLAIM_TTL_SECS)
            .arg(INBOX_TTL_SECS)
            .arg(u8::from(close))
            .arg(self.quota.activations)
            .arg(2 * cypher_crypto::inbox::INBOX_EPOCH_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(items.map(|items| items.into_iter().map(Bytes::from).collect()))
    }

    pub(crate) async fn inbox_ack(
        &self,
        inbox: &[u8; 32],
        claim: &[u8; 16],
    ) -> redis::RedisResult<()> {
        let now = crate::now_secs();
        let _: i64 = self
            .ack_script
            .key(key(b"i:", inbox))
            .key(key(b"c:", inbox))
            .key(key(b"b:", inbox))
            .key(self.quota.day_counters(now))
            .arg(hex::encode(claim))
            .arg(now)
            .arg(INBOX_ITEM_OVERHEAD)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(())
    }

    /// The day counters of all inboxes that exist: what each counts, and
    /// the seconds until it expires.
    #[cfg(test)]
    pub(crate) async fn inbox_counters(&self) -> Vec<(i64, i64)> {
        let mut redis = self.redis.clone();
        let mut counters = Vec::new();
        for counter in self.quota.day_counters(crate::now_secs()) {
            let held: Option<i64> = redis.get(&counter).await.unwrap();
            if let Some(held) = held {
                counters.push((held, redis.ttl(&counter).await.unwrap()));
            }
        }
        counters
    }
}
