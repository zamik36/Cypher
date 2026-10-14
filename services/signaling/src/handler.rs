use bytes::Bytes;
use cypher_crypto::{DeviceList, PrekeyBundle};
use cypher_types::{Addr, LinkId, PeerId};
use cypher_wire::{ClientMsg, ErrorCode, Frame, MAX_INBOX_BATCH, ServerMsg, inbox_id};
use rand::RngCore;
use tracing::warn;

use crate::push::{Outcome, Subscription, WebPush};
use crate::store::{PutOutcome, Store};

pub(crate) const CAPABILITY_ONION: u32 = 1;
pub(crate) const CAPABILITY_PUSH: u32 = 2;
/// At most one signal per inbox in this many seconds: a burst of messages
/// wakes the device once, and a flood cannot turn into a push flood.
const PUSH_INTERVAL_SECS: u64 = 30;

/// Rate limits per identity, in events per hour. Identities are free to
/// create, so these bound one key's cost rather than an attacker's total;
/// the per-target prekey limit is what protects a victim.
const LINKS_PER_HOUR: u32 = 10;
const KEY_FETCHES_PER_HOUR: u32 = 60;
/// One-time prekeys handed out per target and hour. Beyond it the bundle
/// comes without one: pairing still works (forward secrecy then rests on
/// the signed prekey) but a victim's keys cannot be drained quickly.
const OPKS_PER_TARGET_PER_HOUR: u32 = 20;
/// Device lists one identity may look up per hour: its contacts' and its
/// own, refreshed now and then.
const DEVICE_FETCHES_PER_HOUR: u32 = 120;
/// Device lists one identity may publish per hour: linking and unlinking
/// are rare.
const DEVICE_PUBLISHES_PER_HOUR: u32 = 10;
/// Lookups of one invite per hour, whoever asks: only those it was given to
/// know the code, so this cannot be used to make a stranger's invite fail.
const RESOLVES_PER_LINK_PER_HOUR: u32 = 20;
/// Invites one identity may look up per hour.
const RESOLVES_PER_HOUR: u32 = 60;
const HOUR_SECS: u64 = 3600;

pub(crate) struct Handler {
    pub store: Store,
    pub onion_public: [u8; 32],
    pub relay_addr: Option<String>,
    pub push: Option<Pusher>,
}

/// Sends signals: through Web Push, or into a list in tests.
#[derive(Clone)]
pub(crate) enum Pusher {
    Web(std::sync::Arc<WebPush>),
    #[cfg(test)]
    Record(
        std::sync::Arc<std::sync::Mutex<Vec<Subscription>>>,
        [u8; 65],
        Outcome,
    ),
}

impl Pusher {
    fn public_key(&self) -> [u8; 65] {
        match self {
            Self::Web(web) => web.public_key(),
            #[cfg(test)]
            Self::Record(_, key, _) => *key,
        }
    }

    fn accepts(&self, endpoint: &str) -> bool {
        match self {
            Self::Web(web) => web.policy().check(endpoint).is_some(),
            #[cfg(test)]
            Self::Record(..) => endpoint.starts_with("https://"),
        }
    }

    async fn send(&self, sub: &Subscription) -> Outcome {
        match self {
            Self::Web(web) => web.send(sub).await,
            #[cfg(test)]
            Self::Record(sent, _, outcome) => {
                if let Ok(mut sent) = sent.lock() {
                    sent.push(sub.clone());
                }
                *outcome
            }
        }
    }
}

impl Handler {
    /// Answers one client request. `peer` is the gateway-authenticated
    /// identity, absent for anonymous (onion) requests.
    pub(crate) async fn handle(&self, peer: Option<PeerId>, frame: Bytes) -> Bytes {
        let (req_id, msg) = match Frame::<ClientMsg>::decode(frame) {
            Ok(Frame { req_id, msg }) => (req_id, self.answer(peer, msg).await),
            Err(_) => (0, error(ErrorCode::BadRequest)),
        };
        Frame::new(req_id, msg).encode()
    }

    async fn answer(&self, peer: Option<PeerId>, msg: ClientMsg) -> ServerMsg {
        let result = match peer {
            Some(peer) => self.answer_session(&peer, msg).await,
            None => self.answer_anonymous(msg).await,
        };
        result.unwrap_or_else(|e| {
            warn!("redis error: {e}");
            error(ErrorCode::Unavailable)
        })
    }

    /// A request from an identity signed in at a gateway.
    async fn answer_session(&self, peer: &PeerId, msg: ClientMsg) -> redis::RedisResult<ServerMsg> {
        match msg {
            ClientMsg::PublishKeys {
                base,
                opks,
                replace_opks,
            } => self.publish_keys(peer, &base, &opks, replace_opks).await,
            ClientMsg::FetchKeys {
                peer: target,
                device,
            } => self.fetch_keys(Some(peer), Addr::new(target, device)).await,
            ClientMsg::PublishDevices { list } => self.publish_devices(peer, &list).await,
            ClientMsg::FetchDevices { peer: target } => {
                self.fetch_devices(Some(peer), &target).await
            }
            ClientMsg::CreateLink => self.create_link(peer).await,
            ClientMsg::ResolveLink { link } => self.resolve_link(Some(peer), &link).await,
            ClientMsg::Bootstrap => Ok(ServerMsg::BootstrapInfo {
                relay_addr: self.relay_addr.clone().unwrap_or_default(),
                onion_key: self.onion_public,
                capabilities: self.capabilities(),
            }),
            msg @ (ClientMsg::InboxPut { .. }
            | ClientMsg::InboxFetch { .. }
            | ClientMsg::InboxAck { .. }
            | ClientMsg::PushKey
            | ClientMsg::PushRegister { .. }
            | ClientMsg::PushUnregister { .. }) => self.answer_either(Some(*peer), msg).await,
            ClientMsg::Hello { .. }
            | ClientMsg::Auth { .. }
            | ClientMsg::Ping
            | ClientMsg::Send { .. } => Ok(error(ErrorCode::BadRequest)),
        }
    }

    /// A request through the onion relay, from no one in particular: inbox
    /// and push requests, and introductions, which are limited by what they
    /// ask for rather than by who asks.
    async fn answer_anonymous(&self, msg: ClientMsg) -> redis::RedisResult<ServerMsg> {
        match msg {
            ClientMsg::FetchKeys { peer, device } => {
                self.fetch_keys(None, Addr::new(peer, device)).await
            }
            ClientMsg::FetchDevices { peer } => self.fetch_devices(None, &peer).await,
            ClientMsg::ResolveLink { link } => self.resolve_link(None, &link).await,
            msg @ (ClientMsg::InboxPut { .. }
            | ClientMsg::InboxFetch { .. }
            | ClientMsg::InboxAck { .. }
            | ClientMsg::PushKey
            | ClientMsg::PushRegister { .. }
            | ClientMsg::PushUnregister { .. }) => self.answer_either(None, msg).await,
            ClientMsg::PublishKeys { .. }
            | ClientMsg::PublishDevices { .. }
            | ClientMsg::CreateLink
            | ClientMsg::Bootstrap => Ok(error(ErrorCode::Unauthorized)),
            ClientMsg::Hello { .. }
            | ClientMsg::Auth { .. }
            | ClientMsg::Ping
            | ClientMsg::Send { .. } => Ok(error(ErrorCode::BadRequest)),
        }
    }

    /// Inbox and push requests, which name a blind inbox rather than an
    /// identity and are answered the same on either path.
    async fn answer_either(
        &self,
        peer: Option<PeerId>,
        msg: ClientMsg,
    ) -> redis::RedisResult<ServerMsg> {
        match msg {
            ClientMsg::InboxPut { inbox, item } => self.inbox_put(&inbox, &item).await,
            ClientMsg::InboxFetch { secret } => self.inbox_fetch(&secret).await,
            ClientMsg::InboxAck { secret, claim } => self
                .store
                .inbox_ack(&inbox_id(&secret), &claim)
                .await
                .map(|()| ServerMsg::Done),
            msg => self.push_request(peer, msg).await,
        }
    }

    async fn push_request(
        &self,
        peer: Option<PeerId>,
        msg: ClientMsg,
    ) -> redis::RedisResult<ServerMsg> {
        match (msg, peer) {
            (ClientMsg::PushKey, _) => Ok(self.push.as_ref().map_or_else(
                || error(ErrorCode::Unavailable),
                |push| ServerMsg::PushKey {
                    key: push.public_key(),
                },
            )),
            // Only anonymously: a session would tie the device's push
            // address to its identity.
            (ClientMsg::PushRegister { .. } | ClientMsg::PushUnregister { .. }, Some(_)) => {
                Ok(error(ErrorCode::BadRequest))
            }
            (
                ClientMsg::PushRegister {
                    secret,
                    endpoint,
                    p256dh,
                    auth,
                },
                None,
            ) => {
                let sub = Subscription {
                    endpoint,
                    p256dh,
                    auth,
                };
                self.push_register(&secret, &sub).await
            }
            (ClientMsg::PushUnregister { secret }, None) => self
                .store
                .push_unregister(&inbox_id(&secret))
                .await
                .map(|()| ServerMsg::Done),
            _ => Ok(error(ErrorCode::BadRequest)),
        }
    }

    fn capabilities(&self) -> u32 {
        let onion = if self.relay_addr.is_some() {
            CAPABILITY_ONION
        } else {
            0
        };
        let push = if self.push.is_some() {
            CAPABILITY_PUSH
        } else {
            0
        };
        onion | push
    }

    async fn push_register(
        &self,
        secret: &[u8; 32],
        sub: &Subscription,
    ) -> redis::RedisResult<ServerMsg> {
        let Some(push) = &self.push else {
            return Ok(error(ErrorCode::Unavailable));
        };
        if !push.accepts(&sub.endpoint) || p256::PublicKey::from_sec1_bytes(&sub.p256dh).is_err() {
            return Ok(error(ErrorCode::BadRequest));
        }
        self.store
            .push_register(&inbox_id(secret), &sub.to_bytes())
            .await?;
        Ok(ServerMsg::Done)
    }

    /// The inbox got an item: tell its owner's device, at most once per
    /// [`PUSH_INTERVAL_SECS`], without holding up the reply.
    async fn signal(&self, inbox: &[u8; 32]) -> redis::RedisResult<()> {
        let Some(push) = self.push.clone() else {
            return Ok(());
        };
        let Some(sub) = self
            .store
            .push_subscription(inbox)
            .await?
            .and_then(|raw| Subscription::from_bytes(&raw))
        else {
            return Ok(());
        };
        let bucket = [&b"pn:"[..], inbox].concat();
        if !self.store.admit(&bucket, 1, PUSH_INTERVAL_SECS).await? {
            return Ok(());
        }
        let (store, inbox) = (self.store.clone(), *inbox);
        let task = async move {
            if push.send(&sub).await == Outcome::Gone
                && let Err(e) = store.push_unregister(&inbox).await
            {
                warn!("forgetting a push subscription failed: {e}");
            }
        };
        #[cfg(test)]
        task.await;
        #[cfg(not(test))]
        tokio::spawn(task);
        Ok(())
    }

    async fn publish_keys(
        &self,
        peer: &PeerId,
        base: &[u8],
        opks: &[(u32, [u8; 32])],
        replace: bool,
    ) -> redis::RedisResult<ServerMsg> {
        let mut full = base.to_vec();
        full.push(0);
        // Any device of the identity may publish; the bundle names which.
        let device = PrekeyBundle::decode(&full)
            .ok()
            .filter(|b| b.identity == *peer && b.verify().is_ok())
            .map(|b| b.device);
        let Some(device) = device else {
            return Ok(error(ErrorCode::BadRequest));
        };
        let addr = Addr::new(*peer, device);
        let opks_left = self.store.publish_keys(addr, base, opks, replace).await?;
        Ok(ServerMsg::KeysAck { opks_left })
    }

    /// A device's keys. One identity asking is limited per hour; through the
    /// onion only the one-time prekeys are rationed, per target, since a
    /// hard limit per target would let anyone stop a victim's new contacts.
    async fn fetch_keys(
        &self,
        requester: Option<&PeerId>,
        target: Addr,
    ) -> redis::RedisResult<ServerMsg> {
        if let Some(requester) = requester
            && !self.admit(b"fk:", requester, KEY_FETCHES_PER_HOUR).await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        // One-time prekeys are spent per device, so they are rationed per
        // device too.
        let device = target.device.0.to_le_bytes();
        let bucket = [b"ok:", target.peer.as_bytes().as_slice(), &device].concat();
        let with_opk = self
            .store
            .admit(&bucket, OPKS_PER_TARGET_PER_HOUR, HOUR_SECS)
            .await?;
        Ok(match self.store.fetch_keys(target, with_opk).await? {
            Some((base, opk)) => ServerMsg::Keys { base, opk },
            None => error(ErrorCode::NotFound),
        })
    }

    /// Replaces `peer`'s device list with a newer one it signed, and forgets
    /// the keys of the devices it dropped.
    async fn publish_devices(&self, peer: &PeerId, list: &[u8]) -> redis::RedisResult<ServerMsg> {
        let Ok(new) = DeviceList::decode(list) else {
            return Ok(error(ErrorCode::BadRequest));
        };
        if new.identity() != *peer {
            return Ok(error(ErrorCode::BadRequest));
        }
        if !self.admit(b"pd:", peer, DEVICE_PUBLISHES_PER_HOUR).await? {
            return Ok(error(ErrorCode::RateLimited));
        }
        let old = self.store.fetch_devices(peer).await?;
        if !self
            .store
            .publish_devices(peer, new.version(), list)
            .await?
        {
            return Ok(error(ErrorCode::Conflict));
        }
        // Best effort: keys expire on their own anyway.
        let old = old.and_then(|o| DeviceList::decode(&o).ok());
        for &device in old.iter().flat_map(DeviceList::devices) {
            if !new.contains(device) {
                self.store
                    .forget_device_keys(Addr::new(*peer, device))
                    .await?;
            }
        }
        Ok(ServerMsg::Done)
    }

    /// An identity's device list, limited per identity asking; through the
    /// onion only the relay's limits apply.
    async fn fetch_devices(
        &self,
        requester: Option<&PeerId>,
        target: &PeerId,
    ) -> redis::RedisResult<ServerMsg> {
        if let Some(requester) = requester
            && !self
                .admit(b"fd:", requester, DEVICE_FETCHES_PER_HOUR)
                .await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        Ok(match self.store.fetch_devices(target).await? {
            Some(list) => ServerMsg::Devices { list },
            None => error(ErrorCode::NotFound),
        })
    }

    async fn create_link(&self, peer: &PeerId) -> redis::RedisResult<ServerMsg> {
        if !self.admit(b"cl:", peer, LINKS_PER_HOUR).await? {
            return Ok(error(ErrorCode::RateLimited));
        }
        let link = LinkId::random(&mut rand::rngs::OsRng);
        Ok(if self.store.create_link(&link, peer).await? {
            ServerMsg::LinkCreated { link }
        } else {
            error(ErrorCode::Internal)
        })
    }

    /// Who made the invite `link`. Bounded per invite, and per identity when
    /// one asks.
    async fn resolve_link(
        &self,
        requester: Option<&PeerId>,
        link: &LinkId,
    ) -> redis::RedisResult<ServerMsg> {
        if let Some(peer) = requester
            && !self.admit(b"rq:", peer, RESOLVES_PER_HOUR).await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        let bucket = [b"rl:".as_slice(), link.as_str().as_bytes()].concat();
        if !self
            .store
            .admit(&bucket, RESOLVES_PER_LINK_PER_HOUR, HOUR_SECS)
            .await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        Ok(match self.store.resolve_link(link).await? {
            Some(peer) => ServerMsg::LinkResolved { peer },
            None => error(ErrorCode::NotFound),
        })
    }

    async fn admit(&self, kind: &[u8], peer: &PeerId, per_hour: u32) -> redis::RedisResult<bool> {
        let bucket = [kind, peer.as_bytes()].concat();
        self.store.admit(&bucket, per_hour, HOUR_SECS).await
    }

    async fn inbox_put(&self, inbox: &[u8; 32], item: &[u8]) -> redis::RedisResult<ServerMsg> {
        Ok(match self.store.inbox_put(inbox, item).await? {
            PutOutcome::Stored => {
                self.signal(inbox).await?;
                ServerMsg::Done
            }
            PutOutcome::Full => error(ErrorCode::TooLarge),
            PutOutcome::Inactive => error(ErrorCode::NotFound),
        })
    }

    async fn inbox_fetch(&self, secret: &[u8; 32]) -> redis::RedisResult<ServerMsg> {
        let mut claim = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut claim);
        let items = self
            .store
            .inbox_fetch(&inbox_id(secret), &claim, MAX_INBOX_BATCH)
            .await?;
        Ok(ServerMsg::InboxBatch { claim, items })
    }
}

fn error(code: ErrorCode) -> ServerMsg {
    ServerMsg::Error { code }
}
