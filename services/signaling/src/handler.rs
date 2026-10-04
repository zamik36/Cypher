use bytes::Bytes;
use cypher_crypto::PrekeyBundle;
use cypher_types::{LinkId, PeerId};
use cypher_wire::{ClientMsg, ErrorCode, Frame, MAX_INBOX_BATCH, ServerMsg, inbox_id};
use rand::RngCore;
use tracing::warn;

use crate::store::{PutOutcome, Store};

pub(crate) const CAPABILITY_ONION: u32 = 1;

/// Rate limits per identity, in events per hour. Identities are free to
/// create, so these bound one key's cost rather than an attacker's total;
/// the per-target prekey limit is what protects a victim.
const LINKS_PER_HOUR: u32 = 10;
const KEY_FETCHES_PER_HOUR: u32 = 60;
/// One-time prekeys handed out per target and hour. Beyond it the bundle
/// comes without one: pairing still works (forward secrecy then rests on
/// the signed prekey) but a victim's keys cannot be drained quickly.
const OPKS_PER_TARGET_PER_HOUR: u32 = 20;
const HOUR_SECS: u64 = 3600;

pub(crate) struct Handler {
    pub store: Store,
    pub onion_public: [u8; 32],
    pub relay_addr: Option<String>,
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
        let result = match (msg, peer) {
            (
                ClientMsg::PublishKeys {
                    base,
                    opks,
                    replace_opks,
                },
                Some(peer),
            ) => self.publish_keys(&peer, &base, &opks, replace_opks).await,
            (ClientMsg::FetchKeys { peer: target }, Some(peer)) => {
                self.fetch_keys(&peer, &target).await
            }
            (ClientMsg::CreateLink, Some(peer)) => self.create_link(&peer).await,
            (ClientMsg::ResolveLink { link }, Some(_)) => self.resolve_link(&link).await,
            (ClientMsg::InboxPut { inbox, item }, _) => self.inbox_put(&inbox, &item).await,
            (ClientMsg::InboxFetch { secret }, _) => self.inbox_fetch(&secret).await,
            (ClientMsg::InboxAck { secret, claim }, _) => self
                .store
                .inbox_ack(&inbox_id(&secret), &claim)
                .await
                .map(|()| ServerMsg::Done),
            (ClientMsg::Bootstrap, Some(_)) => Ok(ServerMsg::BootstrapInfo {
                relay_addr: self.relay_addr.clone().unwrap_or_default(),
                onion_key: self.onion_public,
                capabilities: if self.relay_addr.is_some() {
                    CAPABILITY_ONION
                } else {
                    0
                },
            }),
            (_, None) => Ok(error(ErrorCode::Unauthorized)),
            _ => Ok(error(ErrorCode::BadRequest)),
        };
        result.unwrap_or_else(|e| {
            warn!("redis error: {e}");
            error(ErrorCode::Unavailable)
        })
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
        let valid =
            PrekeyBundle::decode(&full).is_ok_and(|b| b.identity == *peer && b.verify().is_ok());
        if !valid {
            return Ok(error(ErrorCode::BadRequest));
        }
        let opks_left = self.store.publish_keys(peer, base, opks, replace).await?;
        Ok(ServerMsg::KeysAck { opks_left })
    }

    async fn fetch_keys(
        &self,
        requester: &PeerId,
        target: &PeerId,
    ) -> redis::RedisResult<ServerMsg> {
        if !self.admit(b"fk:", requester, KEY_FETCHES_PER_HOUR).await? {
            return Ok(error(ErrorCode::RateLimited));
        }
        let with_opk = self.admit(b"ok:", target, OPKS_PER_TARGET_PER_HOUR).await?;
        Ok(match self.store.fetch_keys(target, with_opk).await? {
            Some((base, opk)) => ServerMsg::Keys { base, opk },
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

    async fn resolve_link(&self, link: &LinkId) -> redis::RedisResult<ServerMsg> {
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
            PutOutcome::Stored => ServerMsg::Done,
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
