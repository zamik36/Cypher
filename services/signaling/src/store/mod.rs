//! Redis persistence. Keys are short binary prefixes plus raw ids.

use bytes::Bytes;
use cypher_types::{Addr, DeviceId, LinkId, PeerId};
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Script};

mod keyspace;

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
/// What an open inbox costs Redis even while empty (its opening key and
/// expiry entry, rounded up), charged against [`INBOX_BUDGET_BYTES`] for as
/// long as it stays open. Anyone with a fresh key can open an inbox, so
/// openings draw on the budget items do: a flood of them refuses new
/// openings only once it holds as much as a flood of items would.
pub(crate) const INBOX_OPENING_COST: u64 = 200;
const DAY_SECS: u64 = 24 * 3600;
/// What inboxes hold is counted on the day it expires, in a counter that
/// expires at that day's end: today and every day an inbox can live on.
/// A count thus outlives what it counts by less than a day, whether that
/// was read or expired unread.
const BUDGET_DAYS: u64 = INBOX_TTL_SECS / DAY_SECS + 1;

/// Lua for the inbox scripts, which keep the day counters of the budget:
/// the keys from `first` on, one per day from the day `now` falls on.
const BUDGET: &str = include_str!("lua/budget.lua");
/// KEYS: items, byte count, opening, day counters. ARGV: item, max items,
/// max bytes, ttl, budget, now, item overhead. Neither key's expiry moves
/// on a write, so a flood cannot keep an inbox alive.
const PUT_SCRIPT: &str = include_str!("lua/put.lua");
/// KEYS: items, claim, byte count, day counters. ARGV: claim hex, now,
/// item overhead. The claim names the list's expiry: a list made anew
/// since (the old one expired) holds other items, and is left alone.
const ACK_SCRIPT: &str = include_str!("lua/ack.lua");
/// KEYS: items, claim, opening, push, day counters. ARGV: claim hex, max
/// items, max bytes, claim ttl, inbox ttl, close, budget, now, opening
/// cost. Reading (as the owner) is what keeps an inbox open for writers,
/// and its opening counted until it closes or expires; closing shuts it
/// to writers and drops its push subscription at once. An inbox not open
/// yet is opened only within the budget; past it the answer is nil and
/// nothing changes.
const FETCH_SCRIPT: &str = include_str!("lua/fetch.lua");
/// KEYS: list, version. ARGV: version (fixed-width decimal, so string order
/// is number order), list, ttl. Only a newer version replaces the stored one.
const DEVICES_SCRIPT: &str = include_str!("lua/devices.lua");
/// KEYS: unlisted devices, scored in the order they last published. ARGV:
/// device, max devices, ttl. Notes the device as the latest; when `max`
/// others are noted already, the one that published longest ago makes
/// room and is returned (0: none).
const UNLISTED_SCRIPT: &str = include_str!("lua/unlisted.lua");
/// A fixed-window counter. KEYS: bucket. ARGV: limit, window.
const ADMIT_SCRIPT: &str = include_str!("lua/admit.lua");

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
    /// Prefix of the shared counters' keys, so tests keep their own.
    pub scope: Vec<u8>,
}

impl Default for InboxQuota {
    fn default() -> Self {
        Self {
            budget: INBOX_BUDGET_BYTES,
            scope: Vec::new(),
        }
    }
}

impl InboxQuota {
    /// The day counters of inbox bytes, from the day `now` falls on.
    fn day_counters(&self, now: u64) -> Vec<Vec<u8>> {
        let today = now / DAY_SECS;
        (today..today.saturating_add(BUDGET_DAYS))
            .map(|day| keyspace::budget_counter(&self.scope, day))
            .collect()
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
            put_script: Script::new(&[BUDGET, PUT_SCRIPT].concat()),
            fetch_script: Script::new(&[BUDGET, FETCH_SCRIPT].concat()),
            ack_script: Script::new(&[BUDGET, ACK_SCRIPT].concat()),
            admit_script: Script::new(ADMIT_SCRIPT),
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
        let opk_key = keyspace::opks(addr);
        let mut pipe = redis::pipe();
        pipe.atomic()
            .set_ex(keyspace::bundle(addr), base, KEYS_TTL_SECS)
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
        pipe.expire(keyspace::devices(&addr.peer), ttl)
            .ignore()
            .expire(keyspace::device_version(&addr.peer), ttl)
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
        let base_key = keyspace::bundle(addr);
        let (base, opk): (Option<Vec<u8>>, Option<Vec<u8>>) = if with_opk {
            redis::pipe()
                .get(base_key)
                .lpop(keyspace::opks(addr), None)
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
            .key(keyspace::devices(peer))
            .key(keyspace::device_version(peer))
            .arg(format!("{version:020}"))
            .arg(list)
            .arg(KEYS_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(stored == 1)
    }

    /// `peer`'s signed device list, as published.
    pub(crate) async fn fetch_devices(&self, peer: &PeerId) -> redis::RedisResult<Option<Bytes>> {
        let list: Option<Vec<u8>> = self.redis.clone().get(keyspace::devices(peer)).await?;
        Ok(list.map(Bytes::from))
    }

    /// Notes that `addr` holds keys though its identity's list does not
    /// name it. With `max` such devices noted already, the one that
    /// published longest ago is no longer, and is returned: its keys go.
    pub(crate) async fn note_unlisted(
        &self,
        addr: Addr,
        max: usize,
    ) -> redis::RedisResult<Option<DeviceId>> {
        let evicted: u32 = self
            .unlisted_script
            .key(keyspace::unlisted(&addr.peer))
            .arg(addr.device.0)
            .arg(max)
            .arg(KEYS_TTL_SECS)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok((evicted != 0).then_some(DeviceId(evicted)))
    }

    /// The devices of `peer` that hold keys though its list did not name
    /// them, forgotten as such.
    pub(crate) async fn take_unlisted(&self, peer: &PeerId) -> redis::RedisResult<Vec<DeviceId>> {
        let unlisted = keyspace::unlisted(peer);
        let (devices,): (Vec<u32>,) = redis::pipe()
            .atomic()
            .zrange(&unlisted, 0, -1)
            .del(&unlisted)
            .ignore()
            .query_async(&mut self.redis.clone())
            .await?;
        Ok(devices.into_iter().map(DeviceId).collect())
    }

    /// Forgets the keys of a device its identity no longer lists.
    pub(crate) async fn forget_device_keys(&self, addr: Addr) -> redis::RedisResult<()> {
        let keys = [keyspace::bundle(addr), keyspace::opks(addr)];
        self.redis.clone().del(&keys).await
    }

    pub(crate) async fn create_link(
        &self,
        link: &LinkId,
        peer: &PeerId,
    ) -> redis::RedisResult<bool> {
        let mut redis = self.redis.clone();
        redis::cmd("SET")
            .arg(keyspace::link(link))
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
        redis.get(keyspace::onion(name)).await
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
            .arg(keyspace::onion(name))
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
            .arg(keyspace::replay(replay_id))
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(REPLAY_TTL_SECS)
            .query_async::<Option<String>>(&mut redis)
            .await
            .map(|r| r.is_some())
    }

    pub(crate) async fn resolve_link(&self, link: &LinkId) -> redis::RedisResult<Option<PeerId>> {
        let raw: Option<Vec<u8>> = self.redis.clone().get(keyspace::link(link)).await?;
        Ok(raw.and_then(|r| PeerId::from_bytes(&r)))
    }

    /// Seconds left before `prefix ‖ id` expires.
    #[cfg(test)]
    pub(crate) async fn ttl(&self, prefix: &[u8], id: &[u8]) -> i64 {
        self.redis
            .clone()
            .ttl(keyspace::custom(prefix, id))
            .await
            .unwrap()
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
            .key(keyspace::rate_limit(bucket))
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
            .arg(keyspace::push(inbox))
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
        self.redis.clone().get(keyspace::push(inbox)).await
    }

    pub(crate) async fn push_unregister(&self, inbox: &[u8; 32]) -> redis::RedisResult<()> {
        self.redis.clone().del(keyspace::push(inbox)).await
    }

    /// Puts `item` in `inbox`, at `now` (unix seconds).
    pub(crate) async fn inbox_put(
        &self,
        inbox: &[u8; 32],
        item: &[u8],
        now: u64,
    ) -> redis::RedisResult<PutOutcome> {
        let stored: i64 = self
            .put_script
            .key(keyspace::inbox_items(inbox))
            .key(keyspace::inbox_bytes(inbox))
            .key(keyspace::inbox_opening(inbox))
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
    /// open and all inboxes together hold their budget already.
    pub(crate) async fn inbox_fetch(
        &self,
        inbox: &[u8; 32],
        claim: &[u8; 16],
        max_items: usize,
        close: bool,
        now: u64,
    ) -> redis::RedisResult<Option<Vec<Bytes>>> {
        let items: Option<Vec<Vec<u8>>> = self
            .fetch_script
            .key(keyspace::inbox_items(inbox))
            .key(keyspace::inbox_claim(inbox))
            .key(keyspace::inbox_opening(inbox))
            .key(keyspace::push(inbox))
            .key(self.quota.day_counters(now))
            .arg(hex::encode(claim))
            .arg(max_items)
            .arg(MAX_BATCH_BYTES)
            .arg(CLAIM_TTL_SECS)
            .arg(INBOX_TTL_SECS)
            .arg(u8::from(close))
            .arg(self.quota.budget)
            .arg(now)
            .arg(INBOX_OPENING_COST)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(items.map(|items| items.into_iter().map(Bytes::from).collect()))
    }

    /// Drops what `claim` took from `inbox`, at `now` (unix seconds).
    pub(crate) async fn inbox_ack(
        &self,
        inbox: &[u8; 32],
        claim: &[u8; 16],
        now: u64,
    ) -> redis::RedisResult<()> {
        let _: i64 = self
            .ack_script
            .key(keyspace::inbox_items(inbox))
            .key(keyspace::inbox_claim(inbox))
            .key(keyspace::inbox_bytes(inbox))
            .key(self.quota.day_counters(now))
            .arg(hex::encode(claim))
            .arg(now)
            .arg(INBOX_ITEM_OVERHEAD)
            .invoke_async(&mut self.redis.clone())
            .await?;
        Ok(())
    }

    /// The day counters of the budget that exist, as seen at `now`: what
    /// each counts, and the seconds until it expires.
    #[cfg(test)]
    pub(crate) async fn inbox_counters(&self, now: u64) -> Vec<(i64, i64)> {
        let mut redis = self.redis.clone();
        let mut counters = Vec::new();
        for counter in self.quota.day_counters(now) {
            let held: Option<i64> = redis.get(&counter).await.unwrap();
            if let Some(held) = held {
                counters.push((held, redis.ttl(&counter).await.unwrap()));
            }
        }
        counters
    }
}
