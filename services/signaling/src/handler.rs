use bytes::Bytes;
use cypher_crypto::inbox::{InboxOp, push_extra, verify_op};
use cypher_crypto::{DeviceList, PrekeyBundle};
use cypher_types::{Addr, DeviceId, LinkId, PeerId};
use cypher_wire::{
    ClientMsg, DeliveryStatus, ErrorCode, Frame, InboxProof, MAX_INBOX_BATCH, ServerMsg,
    access_hash,
};
use rand::RngCore;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

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
/// Key publishes one identity may make per hour, all its devices together:
/// each device publishes on every connect and when it runs low on one-time
/// prekeys, so this leaves room for flaky mobile links.
const KEY_PUBLISHES_PER_HOUR: u32 = 60;
/// Devices of one identity that may hold keys while its list does not name
/// them: one being linked or recovered, and one spare for a second at once.
/// Publishing a list takes the keys of those it does not name.
const UNLISTED_DEVICES_WITH_KEYS: usize = 2;
/// Inbox items one identity may put per hour over its session, all its
/// devices together (through the onion the relay limits them): a message
/// to a contact who is away goes to each of its devices and to this
/// identity's own, so a busy writer puts a few hundred.
const INBOX_PUTS_PER_HOUR: u32 = 1200;
/// Inbox reads one identity may make per hour over its session: each
/// device polls two to four inboxes every five minutes and reads a full
/// one batch by batch.
const INBOX_FETCHES_PER_HOUR: u32 = 600;
/// Lookups of one invite per hour, whoever asks: only those it was given to
/// know the code, so this cannot be used to make a stranger's invite fail.
const RESOLVES_PER_LINK_PER_HOUR: u32 = 20;
/// Invites one identity may look up per hour.
const RESOLVES_PER_HOUR: u32 = 60;
const HOUR_SECS: u64 = 3600;
/// How far the time a signed inbox request names may be from the server's.
const INBOX_CLOCK_SKEW_SECS: u64 = 120;
/// Sealed sends per access token and hour: the token is one device's for
/// a week and shared by its contacts, so this bounds what they can push
/// through to it together; beyond it they send as themselves.
const SEALED_PER_TOKEN_PER_HOUR: u32 = 3600;
/// Inbox fetches answered at once. A batch holds up to 768 KiB, so this
/// bounds that memory (~48 MiB) while the other requests, all small, may
/// be many more.
pub(crate) const FETCH_PERMITS: usize = 64;
/// Push signals sent at once. Each is one outbound HTTPS request of up to
/// 20 s (the push request timeout); beyond this many a flood of inbox
/// puts would turn into a flood of requests, and push services ban VAPID
/// keys that flood them. A signal past the limit is dropped: the device
/// still finds the item on its next poll.
pub(crate) const PUSH_PERMITS: usize = 256;
/// How long the recipient's gateway has to take a sealed delivery.
const SEALED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub(crate) struct Handler {
    pub store: Store,
    /// The long-term key clients remember this server by.
    pub signing_key: [u8; 32],
    pub relay_addr: Option<String>,
    pub push: Option<Pusher>,
    pub courier: Courier,
    pub fetches: Semaphore,
    /// Push signals on their way, at most [`PUSH_PERMITS`]; each task holds
    /// a permit until its push service answered or timed out.
    pub pushes: std::sync::Arc<Semaphore>,
    /// Push signals dropped because [`PUSH_PERMITS`] were on their way.
    pub push_shed: prometheus::IntCounter,
    /// Sealed sends no gateway answered: the way to the gateways is broken
    /// (NATS down or refusing), and senders fall back to their name.
    pub sealed_unavailable: prometheus::IntCounter,
    /// Inbox writes and openings refused because all inboxes together hold
    /// their budget or this epoch's openings: someone floods them, or the
    /// budget is too small for the users.
    pub inbox_quota_refused: prometheus::IntCounter,
}

/// Hands sealed sends to the gateway of the device they are for: through
/// NATS, or to a stand-in in tests.
pub(crate) enum Courier {
    Nats(async_nats::Client),
    #[cfg(test)]
    Stub(std::sync::Arc<dyn Fn(Addr, Bytes) -> Delivery + Send + Sync>),
}

/// What became of a sealed delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// The device's gateway answered with this status byte.
    Answered(u8),
    /// No gateway holds the device: it is away.
    Away,
    /// The gateway did not answer in time.
    Unavailable,
}

impl Courier {
    async fn deliver(&self, to: Addr, payload: Bytes) -> Delivery {
        match self {
            Self::Nats(nats) => {
                let subject = cypher_server_kit::peer_subject(
                    &to.peer.to_hex(),
                    to.device,
                    cypher_server_kit::Origin::Sealed,
                );
                let request = async_nats::Request::new()
                    .payload(payload)
                    .timeout(Some(SEALED_TIMEOUT));
                match nats.send_request(subject, request).await {
                    Ok(reply) => reply
                        .payload
                        .first()
                        .map_or(Delivery::Unavailable, |status| Delivery::Answered(*status)),
                    Err(e) if e.kind() == async_nats::RequestErrorKind::NoResponders => {
                        Delivery::Away
                    }
                    Err(_) => Delivery::Unavailable,
                }
            }
            #[cfg(test)]
            Self::Stub(deliver) => deliver(to, payload),
        }
    }
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

/// Who a request comes from, as far as signaling knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    /// The identity signed in at the gateway that forwarded the request.
    Session(PeerId),
    /// No one in particular: the request came through the onion relay.
    Onion,
}

/// Whether `msg` may come through the onion, from no one in particular:
/// requests that name a blind inbox, and introductions, which are limited
/// by what they ask for rather than by who asks. Requests that need an
/// identity, and what is not a signaling request at all, are not.
pub(crate) fn anonymous_allowed(msg: &ClientMsg) -> bool {
    match msg {
        ClientMsg::InboxPut { .. }
        | ClientMsg::InboxFetch { .. }
        | ClientMsg::InboxAck { .. }
        | ClientMsg::PushKey
        | ClientMsg::PushRegister { .. }
        | ClientMsg::PushUnregister { .. }
        | ClientMsg::FetchKeys { .. }
        | ClientMsg::FetchDevices { .. }
        | ClientMsg::ResolveLink { .. }
        | ClientMsg::SendSealed { .. } => true,
        ClientMsg::PublishKeys { .. }
        | ClientMsg::PublishDevices { .. }
        | ClientMsg::CreateLink
        | ClientMsg::Bootstrap
        | ClientMsg::Hello { .. }
        | ClientMsg::Auth { .. }
        | ClientMsg::Ping
        | ClientMsg::Send { .. }
        | ClientMsg::SetAccess { .. } => false,
    }
}

/// The reply `code` to an encoded request, echoing its id when it has one.
fn refuse(frame: Bytes, code: ErrorCode) -> Bytes {
    let req_id = Frame::<ClientMsg>::decode(frame).map_or(0, |f| f.req_id);
    Frame::new(req_id, error(code)).encode()
}

impl Handler {
    /// Answers a request a gateway forwarded on `sig.req`, from the identity
    /// its peer header names (`peer_hex`). Without a valid one it is
    /// refused rather than taken as anonymous: only the onion is anonymous.
    pub(crate) async fn handle_forwarded(&self, peer_hex: Option<&str>, frame: Bytes) -> Bytes {
        if let Some(peer) = peer_hex.and_then(PeerId::from_hex) {
            self.handle(Caller::Session(peer), frame).await
        } else {
            debug!("a forwarded request without a valid peer header");
            refuse(frame, ErrorCode::BadRequest)
        }
    }

    /// Answers one encoded request from `caller`.
    pub(crate) async fn handle(&self, caller: Caller, frame: Bytes) -> Bytes {
        match Frame::<ClientMsg>::decode(frame) {
            Ok(frame) => self.handle_frame(caller, frame).await,
            Err(_) => Frame::new(0, error(ErrorCode::BadRequest)).encode(),
        }
    }

    /// Answers one decoded request from `caller`.
    pub(crate) async fn handle_frame(&self, caller: Caller, frame: Frame<ClientMsg>) -> Bytes {
        let Frame { req_id, msg } = frame;
        Frame::new(req_id, self.answer(caller, msg).await).encode()
    }

    async fn answer(&self, caller: Caller, msg: ClientMsg) -> ServerMsg {
        let result = match caller {
            Caller::Session(peer) => self.answer_session(&peer, msg).await,
            Caller::Onion if anonymous_allowed(&msg) => self.answer_anonymous(msg).await,
            Caller::Onion => Ok(error(ErrorCode::Unauthorized)),
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
                signing_key: self.signing_key,
                capabilities: self.capabilities(),
            }),
            msg @ (ClientMsg::InboxPut { .. }
            | ClientMsg::InboxFetch { .. }
            | ClientMsg::InboxAck { .. }
            | ClientMsg::PushKey
            | ClientMsg::PushRegister { .. }
            | ClientMsg::PushUnregister { .. }) => {
                self.answer_either(Caller::Session(*peer), msg).await
            }
            ClientMsg::Hello { .. }
            | ClientMsg::Auth { .. }
            | ClientMsg::Ping
            | ClientMsg::Send { .. }
            | ClientMsg::SendSealed { .. }
            | ClientMsg::SetAccess { .. } => Ok(error(ErrorCode::BadRequest)),
        }
    }

    /// A request through the onion relay, from no one in particular, of a
    /// kind [`anonymous_allowed`] lets through.
    async fn answer_anonymous(&self, msg: ClientMsg) -> redis::RedisResult<ServerMsg> {
        match msg {
            ClientMsg::SendSealed {
                to,
                device,
                token,
                inbox,
                item,
            } => {
                self.send_sealed(Addr::new(to, device), &token, inbox, &item)
                    .await
            }
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
            | ClientMsg::PushUnregister { .. }) => self.answer_either(Caller::Onion, msg).await,
            // Turned away by `anonymous_allowed` before they get here.
            ClientMsg::PublishKeys { .. }
            | ClientMsg::PublishDevices { .. }
            | ClientMsg::CreateLink
            | ClientMsg::Bootstrap
            | ClientMsg::Hello { .. }
            | ClientMsg::Auth { .. }
            | ClientMsg::Ping
            | ClientMsg::Send { .. }
            | ClientMsg::SetAccess { .. } => Ok(error(ErrorCode::Unauthorized)),
        }
    }

    /// Inbox and push requests, which name a blind inbox rather than an
    /// identity and are answered the same on either path.
    async fn answer_either(&self, caller: Caller, msg: ClientMsg) -> redis::RedisResult<ServerMsg> {
        match msg {
            ClientMsg::InboxPut { inbox, item } => {
                if !self
                    .admit_caller(b"ip:", caller, INBOX_PUTS_PER_HOUR)
                    .await?
                {
                    return Ok(error(ErrorCode::RateLimited));
                }
                self.inbox_put(&inbox, &item).await
            }
            ClientMsg::InboxFetch { proof, close } => {
                if !self
                    .admit_caller(b"if:", caller, INBOX_FETCHES_PER_HOUR)
                    .await?
                {
                    return Ok(error(ErrorCode::RateLimited));
                }
                let op = if close {
                    InboxOp::Close
                } else {
                    InboxOp::Fetch
                };
                if !proven(&proof, op, &[]) {
                    return Ok(error(ErrorCode::Unauthorized));
                }
                self.inbox_fetch(&proof.inbox, close).await
            }
            ClientMsg::InboxAck { proof, claim } => {
                if !proven(&proof, InboxOp::Ack, &claim) {
                    return Ok(error(ErrorCode::Unauthorized));
                }
                self.store
                    .inbox_ack(&proof.inbox, &claim)
                    .await
                    .map(|()| ServerMsg::Done)
            }
            msg => self.push_request(caller, msg).await,
        }
    }

    async fn push_request(&self, caller: Caller, msg: ClientMsg) -> redis::RedisResult<ServerMsg> {
        match (msg, caller) {
            (ClientMsg::PushKey, _) => Ok(self.push.as_ref().map_or_else(
                || error(ErrorCode::Unavailable),
                |push| ServerMsg::PushKey {
                    key: push.public_key(),
                },
            )),
            (
                ClientMsg::PushRegister {
                    proof,
                    endpoint,
                    p256dh,
                    auth,
                },
                Caller::Onion,
            ) => {
                let extra = push_extra(&endpoint, &p256dh, &auth);
                if !proven(&proof, InboxOp::PushRegister, &extra) {
                    return Ok(error(ErrorCode::Unauthorized));
                }
                let sub = Subscription {
                    endpoint,
                    p256dh,
                    auth,
                };
                self.push_register(&proof.inbox, &sub).await
            }
            (ClientMsg::PushUnregister { proof }, Caller::Onion) => {
                if !proven(&proof, InboxOp::PushUnregister, &[]) {
                    return Ok(error(ErrorCode::Unauthorized));
                }
                self.store
                    .push_unregister(&proof.inbox)
                    .await
                    .map(|()| ServerMsg::Done)
            }
            // Registering only anonymously: a session would tie the
            // device's push address to its identity.
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
        inbox: &[u8; 32],
        sub: &Subscription,
    ) -> redis::RedisResult<ServerMsg> {
        let Some(push) = &self.push else {
            return Ok(error(ErrorCode::Unavailable));
        };
        if !push.accepts(&sub.endpoint) || p256::PublicKey::from_sec1_bytes(&sub.p256dh).is_err() {
            return Ok(error(ErrorCode::BadRequest));
        }
        self.store.push_register(inbox, &sub.to_bytes()).await?;
        Ok(ServerMsg::Done)
    }

    /// The inbox got an item: tell its owner's device, at most once per
    /// [`PUSH_INTERVAL_SECS`], without holding up the reply. With
    /// [`PUSH_PERMITS`] signals on their way already, this one is dropped.
    async fn signal(&self, inbox: &[u8; 32]) -> redis::RedisResult<()> {
        let Some(push) = self.push.clone() else {
            return Ok(());
        };
        let Ok(permit) = std::sync::Arc::clone(&self.pushes).try_acquire_owned() else {
            self.push_shed.inc();
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
        tokio::spawn(async move {
            if push.send(&sub).await == Outcome::Gone
                && let Err(e) = store.push_unregister(&inbox).await
            {
                warn!("forgetting a push subscription failed: {e}");
            }
            drop(permit);
        });
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
        if !self.admit(b"pk:", peer, KEY_PUBLISHES_PER_HOUR).await? {
            return Ok(error(ErrorCode::RateLimited));
        }
        let addr = Addr::new(*peer, device);
        if !self.may_hold_keys(addr).await? {
            return Ok(error(ErrorCode::BadRequest));
        }
        let opks_left = self.store.publish_keys(addr, base, opks, replace).await?;
        Ok(ServerMsg::KeysAck { opks_left })
    }

    /// Whether the device `addr` may hold keys: its identity lists it (the
    /// first device, before there is a list), or it is one of the few
    /// about to be listed, since a device being linked or recovered
    /// publishes its keys before its list names it.
    async fn may_hold_keys(&self, addr: Addr) -> redis::RedisResult<bool> {
        let list = self.store.fetch_devices(&addr.peer).await?;
        let listed = match list.as_deref().map(DeviceList::decode) {
            Some(Ok(list)) => list.contains(addr.device),
            Some(Err(_)) | None => addr.device == DeviceId::FIRST,
        };
        if listed {
            return Ok(true);
        }
        self.store
            .admit_unlisted(addr, UNLISTED_DEVICES_WITH_KEYS)
            .await
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
        // Best effort: keys expire on their own anyway. Devices that held
        // keys before a list named them count as listed until now.
        let old = old.and_then(|o| DeviceList::decode(&o).ok());
        let unlisted = self.store.take_unlisted(peer).await?;
        let held = old.iter().flat_map(DeviceList::devices).chain(&unlisted);
        for &device in held {
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

    /// [`Self::admit`] for a session's identity; through the onion only
    /// the relay's limits and the quotas of all inboxes apply.
    async fn admit_caller(
        &self,
        kind: &[u8],
        caller: Caller,
        per_hour: u32,
    ) -> redis::RedisResult<bool> {
        match caller {
            Caller::Session(peer) => self.admit(kind, &peer, per_hour).await,
            Caller::Onion => Ok(true),
        }
    }

    async fn inbox_put(&self, inbox: &[u8; 32], item: &[u8]) -> redis::RedisResult<ServerMsg> {
        Ok(match self.store.inbox_put(inbox, item).await? {
            PutOutcome::Stored => {
                self.signal(inbox).await?;
                ServerMsg::Done
            }
            PutOutcome::Full => error(ErrorCode::TooLarge),
            // To the writer as if this inbox were full: it tries again later.
            PutOutcome::OverBudget => {
                self.inbox_quota_refused.inc();
                error(ErrorCode::TooLarge)
            }
            PutOutcome::Inactive => error(ErrorCode::NotFound),
        })
    }

    /// Hands `item` to the device's gateway, which takes it only with a
    /// token the device told it; a device that is away finds it in `inbox`.
    async fn send_sealed(
        &self,
        to: Addr,
        token: &[u8; 32],
        inbox: Option<[u8; 32]>,
        item: &Bytes,
    ) -> redis::RedisResult<ServerMsg> {
        let bucket = [b"ss:".as_slice(), &access_hash(token)].concat();
        if !self
            .store
            .admit(&bucket, SEALED_PER_TOKEN_PER_HOUR, HOUR_SECS)
            .await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        let payload = Bytes::from([token.as_slice(), item].concat());
        let status = match self.courier.deliver(to, payload).await {
            Delivery::Answered(cypher_server_kit::SEALED_REFUSED) => {
                return Ok(error(ErrorCode::Unauthorized));
            }
            Delivery::Answered(status) if status == DeliveryStatus::Delivered as u8 => {
                DeliveryStatus::Delivered
            }
            Delivery::Answered(_) => DeliveryStatus::Busy,
            // The way to the gateway is broken, not the device busy: the
            // client sends as itself for a while instead of waiting.
            Delivery::Unavailable => {
                self.sealed_unavailable.inc();
                return Ok(error(ErrorCode::Unavailable));
            }
            Delivery::Away => match inbox {
                Some(inbox) => {
                    return Ok(match self.inbox_put(&inbox, item).await? {
                        ServerMsg::Done => ServerMsg::SendAck {
                            status: DeliveryStatus::Queued,
                        },
                        refused => refused,
                    });
                }
                None => DeliveryStatus::Offline,
            },
        };
        Ok(ServerMsg::SendAck { status })
    }

    async fn inbox_fetch(&self, inbox: &[u8; 32], close: bool) -> redis::RedisResult<ServerMsg> {
        let Ok(_permit) = self.fetches.try_acquire() else {
            return Ok(error(ErrorCode::Unavailable));
        };
        let mut claim = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut claim);
        let items = self
            .store
            .inbox_fetch(inbox, &claim, MAX_INBOX_BATCH, close)
            .await?;
        // No more inboxes may be opened this epoch: the owner asks again
        // later, and its inbox is not open to writers meanwhile.
        Ok(items.map_or_else(
            || {
                self.inbox_quota_refused.inc();
                error(ErrorCode::Unavailable)
            },
            |items| ServerMsg::InboxBatch { claim, items },
        ))
    }
}

/// Whether the inbox's owner signed request `op` recently enough that it is
/// not an old one played back.
fn proven(proof: &InboxProof, op: InboxOp, extra: &[u8]) -> bool {
    crate::now_secs().abs_diff(proof.timestamp) <= INBOX_CLOCK_SKEW_SECS
        && verify_op(&proof.inbox, op, proof.timestamp, extra, &proof.signature).is_ok()
}

fn error(code: ErrorCode) -> ServerMsg {
    ServerMsg::Error { code }
}
