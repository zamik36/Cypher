mod anon;
mod files;
mod messaging;
mod push;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use bytes::Bytes;
use cypher_crypto::{IdentityKeyPair, IdentitySeed};
use cypher_types::{DeviceId, FileId, MsgId, PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, ErrorCode, Frame, PROTOCOL_VERSION, ServerMsg};
use rand_core::CryptoRngCore;
use zeroize::Zeroizing;

use crate::CoreError;
use crate::api::{Command, Effect, Event, FailReason, Input};
use crate::peer::{OwnLinks, Peer, PeerRecord, ProfileRecord};
use crate::prekeys::{OPK_LOW_WATER, Prekeys, PrekeysRecord};
use crate::share::ShareLink;
use crate::store::{META_LINKS, META_PREKEYS, META_PROFILE, Record, StoreOp, Table, Vault};
use crate::transfer::{Incoming, Outgoing, TransferRecord};

use anon::{Anon, Readiness};
use messaging::OutboxItem;
use push::{PushState, Subscription};

const REQUEST_TIMEOUT_MS: u64 = 15_000;
const PING_INTERVAL_MS: u64 = 20_000;
const DEAD_AFTER_MS: u64 = 50_000;
const INBOX_POLL_MS: u64 = 5 * 60_000;
/// Least time between two republished key sets after unknown prekeys.
const RESYNC_INTERVAL_MS: u64 = 60 * 60_000;
/// Least time between two fresh sessions started with one contact.
const REPAIR_INTERVAL_MS: u64 = 10 * 60_000;
/// How long a contact's device may hold our first message unanswered
/// before we take it that our prekeys were gone and start again.
const INIT_TIMEOUT_MS: u64 = 5 * 60_000;
const RECENT_IDS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conn {
    Offline,
    Authenticating,
    Ready,
}

#[derive(Debug)]
enum Pending {
    CreateLink,
    Resolve {
        link: ShareLink,
    },
    FetchKeys {
        link: String,
        peer: PeerId,
    },
    /// Keys for a fresh session with a contact whose messages stopped
    /// decrypting.
    Repair {
        peer: PeerId,
    },
    Publish {
        batch: bool,
    },
    Send {
        msg_id: MsgId,
        peer: PeerId,
        body: Bytes,
    },
    InboxPut {
        msg_id: MsgId,
    },
    InboxFetch,
    InboxAck,
    Bootstrap,
    PushKey,
    PushRegister,
    PushUnregister,
}

/// What a server error means for the request it answers.
fn failure(code: ErrorCode) -> FailReason {
    match code {
        ErrorCode::NotFound => FailReason::NotFound,
        ErrorCode::Unauthorized => FailReason::Unauthorized,
        ErrorCode::RateLimited | ErrorCode::Unavailable => FailReason::Offline,
        ErrorCode::BadRequest | ErrorCode::TooLarge | ErrorCode::Internal => {
            FailReason::ServerError
        }
        ErrorCode::UnsupportedVersion => FailReason::UpdateRequired,
    }
}

/// Encrypted state restored by the driver at startup: raw `(key, value)`
/// pairs read from the corresponding [`Table`]s.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub meta: Rows,
    pub peers: Rows,
    pub outbox: Rows,
    pub transfers: Rows,
}

/// `(key, value)` rows as read from one storage table.
pub type Rows = Vec<(Vec<u8>, Vec<u8>)>;

/// The sans-IO client state machine. Drivers own sockets, disks and clocks;
/// the core owns every protocol and cryptographic decision.
pub struct Core<R> {
    rng: R,
    now: u64,
    identity: IdentityKeyPair,
    peer_id: PeerId,
    inbox_secret: Zeroizing<[u8; 32]>,
    inbox_id: [u8; 32],
    vault: Vault,
    prekeys: Prekeys,
    peers: HashMap<PeerId, Peer>,
    outbox: BTreeMap<MsgId, OutboxItem>,
    outgoing: HashMap<FileId, Outgoing>,
    incoming: HashMap<FileId, Incoming>,
    pending: HashMap<u32, (Pending, u64)>,
    next_req: u32,
    conn: Conn,
    last_rx: u64,
    last_ping: u64,
    last_inbox_fetch: u64,
    /// When keys were last republished because the server's were not ours.
    last_resync: Option<u64>,
    /// When a fresh session was last started with a contact, per contact.
    last_repair: HashMap<PeerId, u64>,
    /// When a message carrying our unanswered first one reached the
    /// contact's device, per contact.
    init_heard: HashMap<PeerId, u64>,
    recent: RecentIds,
    /// Our messages the contact has read, so a late receipt cannot undo it.
    read: RecentIds,
    progress_at: HashMap<FileId, u64>,
    anon: Anon,
    push: PushState,
    /// The name this user goes by, sent to contacts with `Hello`.
    profile_name: Option<String>,
    /// Invites this user made and when; each admits one contact.
    own_links: Vec<(String, u64)>,
    effects: Vec<Effect>,
}

impl<R: CryptoRngCore> Core<R> {
    /// Builds the core from the identity seed and previously persisted state.
    /// Returns persistence effects when fresh prekeys had to be generated.
    pub fn restore(
        seed: &IdentitySeed,
        snapshot: &Snapshot,
        now_ms: u64,
        mut rng: R,
    ) -> Result<(Self, Vec<Effect>), CoreError> {
        let identity = seed.derive_identity();
        let vault = Vault::new(seed.derive_storage_key());
        let inbox_secret = seed.derive_inbox_secret(DeviceId::FIRST);

        let mut skipped = Skipped::default();
        let stored_prekeys =
            load_meta::<PrekeysRecord>(&vault, &snapshot.meta, META_PREKEYS, &mut skipped)?;
        let fresh_prekeys = stored_prekeys.is_none();
        let prekeys = stored_prekeys.map_or_else(
            || Prekeys::generate(now_ms, &mut rng),
            |r| Prekeys::from_record(&r),
        );

        let profile_name = load_profile(&vault, &snapshot.meta, &mut skipped)?;
        let own_links = load_meta::<OwnLinks>(&vault, &snapshot.meta, META_LINKS, &mut skipped)?
            .map(|record| record.links)
            .unwrap_or_default();
        let peers = load_peers(&vault, &snapshot.peers, &mut skipped)?;
        let outbox = load_outbox(&vault, &snapshot.outbox, &mut skipped)?;
        let (outgoing, incoming) = load_transfers(&vault, &snapshot.transfers, &mut skipped)?;

        let mut core = Self {
            rng,
            now: now_ms,
            peer_id: identity.peer_id(),
            identity,
            inbox_id: cypher_wire::inbox_id(&inbox_secret),
            inbox_secret,
            vault,
            prekeys,
            peers,
            outbox,
            outgoing,
            incoming,
            pending: HashMap::new(),
            next_req: 0,
            conn: Conn::Offline,
            last_rx: now_ms,
            last_ping: now_ms,
            last_inbox_fetch: 0,
            last_resync: None,
            last_repair: HashMap::new(),
            init_heard: HashMap::new(),
            recent: RecentIds::default(),
            read: RecentIds::default(),
            progress_at: HashMap::new(),
            anon: Anon::default(),
            push: PushState::default(),
            profile_name,
            own_links,
            effects: Vec::new(),
        };
        if fresh_prekeys {
            core.persist_prekeys();
        }
        core.announce_pending_offers();
        if skipped.0 > 0 {
            core.emit(Event::Warning {
                reason: FailReason::Corrupted,
            });
        }
        let effects = std::mem::take(&mut core.effects);
        Ok((core, effects))
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn inbox_id(&self) -> [u8; 32] {
        self.inbox_id
    }

    pub fn is_ready(&self) -> bool {
        self.conn == Conn::Ready
    }

    pub fn peers(&self) -> impl Iterator<Item = &PeerId> {
        self.peers.keys()
    }

    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Effect> {
        self.now = now_ms;
        match input {
            Input::Connected => self.on_connected(),
            Input::Disconnected => self.on_disconnected(),
            Input::Frame(bytes) => self.on_frame(bytes, true),
            Input::AnonymousFrame(bytes) => {
                if let Some(frame) = self.anon.open(&bytes) {
                    self.on_frame(frame, false);
                }
            }
            Input::AnonymousChannel { up } => {
                self.anon.set_relay_up(up);
                self.emit(Event::Onion { up });
                if up {
                    self.fetch_inbox();
                    self.renew_push();
                }
            }
            Input::Command(cmd) => self.on_command(cmd),
            Input::ChunkRead {
                file_id,
                index,
                buf,
            } => self.on_chunk_read(file_id, index, buf),
            Input::ChunkUnavailable { file_id } => {
                self.fail_outgoing(&file_id, FailReason::SourceUnavailable);
            }
            Input::Tick => self.on_tick(),
        }
        std::mem::take(&mut self.effects)
    }

    fn on_connected(&mut self) {
        self.conn = Conn::Authenticating;
        self.last_rx = self.now;
        self.transmit(
            0,
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                peer: self.peer_id,
            },
        );
    }

    fn on_disconnected(&mut self) {
        if self.conn == Conn::Offline {
            return;
        }
        self.conn = Conn::Offline;
        self.anon.on_disconnected();
        let pending: Vec<_> = self.pending.drain().map(|(_, (p, _))| p).collect();
        for p in pending {
            self.fail_request(p, FailReason::Offline);
        }
        self.pause_transfers();
        self.emit(Event::Disconnected);
    }

    fn on_ready(&mut self) {
        self.conn = Conn::Ready;
        self.emit(Event::Connected);
        self.publish_keys(false);
        self.request(ClientMsg::Bootstrap, Pending::Bootstrap, false);
        self.last_inbox_fetch = 0;
        self.flush_outbox();
        self.resume_transfers();
    }

    fn on_frame(&mut self, bytes: Bytes, session: bool) {
        let Ok(Frame { req_id, msg }) = Frame::<ServerMsg>::decode(bytes) else {
            if session {
                self.effects.push(Effect::Disconnect { reconnect: true });
            }
            return;
        };
        if !session
            && !matches!(
                msg,
                ServerMsg::InboxBatch { .. }
                    | ServerMsg::Done
                    | ServerMsg::PushKey { .. }
                    | ServerMsg::Error { .. }
            )
        {
            return;
        }
        if session {
            self.last_rx = self.now;
        }
        match msg {
            ServerMsg::Challenge { nonce } => self.on_challenge(&nonce),
            ServerMsg::Ready => self.on_ready(),
            ServerMsg::Pong => {}
            ServerMsg::Superseded => {
                self.conn = Conn::Offline;
                self.emit(Event::Superseded);
                self.effects.push(Effect::Disconnect { reconnect: false });
            }
            ServerMsg::Recv { from, body } => self.on_relay(from, &body, false),
            msg @ (ServerMsg::SendAck { .. }
            | ServerMsg::Keys { .. }
            | ServerMsg::KeysAck { .. }
            | ServerMsg::LinkCreated { .. }
            | ServerMsg::LinkResolved { .. }
            | ServerMsg::InboxBatch { .. }
            | ServerMsg::Done
            | ServerMsg::BootstrapInfo { .. }
            | ServerMsg::PushKey { .. }
            | ServerMsg::Error { .. }) => match self.pending.remove(&req_id) {
                Some((pending, _)) => self.on_response(pending, msg),
                None => {
                    if let ServerMsg::Error { code } = msg {
                        self.on_unsolicited_error(code);
                    }
                }
            },
        }
    }

    /// An error outside any request concerns the session itself.
    fn on_unsolicited_error(&mut self, code: ErrorCode) {
        match code {
            ErrorCode::Unauthorized => self.effects.push(Effect::Disconnect { reconnect: false }),
            // Reconnecting cannot help: only a newer app can talk to this
            // server.
            ErrorCode::UnsupportedVersion => {
                self.conn = Conn::Offline;
                self.emit(Event::Warning {
                    reason: FailReason::UpdateRequired,
                });
                self.effects.push(Effect::Disconnect { reconnect: false });
            }
            ErrorCode::NotFound
            | ErrorCode::BadRequest
            | ErrorCode::RateLimited
            | ErrorCode::TooLarge
            | ErrorCode::Unavailable
            | ErrorCode::Internal => {}
        }
    }

    fn on_challenge(&mut self, nonce: &[u8; 32]) {
        if self.conn != Conn::Authenticating {
            return;
        }
        let mut signed = Vec::with_capacity(SESSION_AUTH_CONTEXT.len() + 32);
        signed.extend_from_slice(SESSION_AUTH_CONTEXT);
        signed.extend_from_slice(nonce);
        let signature = self.identity.sign(&signed).to_bytes();
        self.transmit(0, ClientMsg::Auth { signature });
    }

    fn on_response(&mut self, pending: Pending, msg: ServerMsg) {
        if let ServerMsg::Error { code } = msg {
            self.fail_request(pending, failure(code));
        } else if matches!(
            pending,
            Pending::PushKey | Pending::PushRegister | Pending::PushUnregister
        ) {
            self.on_push_response(&pending, &msg);
        } else {
            self.on_success(pending, msg);
        }
    }

    fn on_success(&mut self, pending: Pending, msg: ServerMsg) {
        match (pending, msg) {
            (Pending::CreateLink, ServerMsg::LinkCreated { link }) => {
                self.remember_link(link.as_str());
                self.emit(Event::LinkCreated {
                    link: ShareLink::new(link, &self.peer_id).to_string(),
                });
            }
            (Pending::Resolve { link }, ServerMsg::LinkResolved { peer }) => {
                self.on_link_resolved(&link, peer);
            }
            (Pending::FetchKeys { link, peer }, ServerMsg::Keys { base, opk }) => {
                self.on_keys(link, peer, &base, opk);
            }
            (Pending::Repair { peer }, ServerMsg::Keys { base, opk }) => {
                self.on_repair_keys(peer, &base, opk);
            }
            (Pending::Publish { batch }, ServerMsg::KeysAck { opks_left }) => {
                if !batch && opks_left < OPK_LOW_WATER {
                    self.publish_keys(true);
                }
            }
            (Pending::Send { msg_id, peer, body }, ServerMsg::SendAck { status }) => {
                self.on_send_ack(msg_id, peer, &body, status);
            }
            (Pending::InboxPut { msg_id }, ServerMsg::Done) => self.on_inbox_queued(msg_id),
            (Pending::InboxFetch, ServerMsg::InboxBatch { claim, items }) => {
                self.on_inbox_batch(claim, items);
            }
            (Pending::InboxAck, ServerMsg::Done) => {}
            (
                Pending::Bootstrap,
                ServerMsg::BootstrapInfo {
                    relay_addr,
                    onion_key,
                    ..
                },
            ) => {
                let relay = (!relay_addr.is_empty()).then_some(onion_key);
                self.anon.on_bootstrap(relay, self.now);
                self.fetch_inbox();
                self.push_step();
                self.emit(Event::Bootstrap {
                    relay_addr,
                    onion_key,
                });
            }
            (pending, _) => self.fail_request(pending, FailReason::ServerError),
        }
    }

    fn fail_request(&mut self, pending: Pending, reason: FailReason) {
        match pending {
            Pending::Resolve { link } => self.emit(Event::JoinFailed {
                link: link.to_string(),
                reason,
            }),
            Pending::FetchKeys { link, .. } => self.emit(Event::JoinFailed { link, reason }),
            Pending::Bootstrap => {
                self.anon.on_bootstrap(None, self.now);
                self.fetch_inbox();
            }
            Pending::CreateLink | Pending::InboxFetch => {
                self.emit(Event::Warning { reason });
            }
            Pending::Send { msg_id, .. } | Pending::InboxPut { msg_id } => {
                self.on_send_failed(msg_id);
            }
            Pending::Publish { .. } | Pending::InboxAck | Pending::Repair { .. } => {}
            Pending::PushKey | Pending::PushRegister | Pending::PushUnregister => {
                self.on_push_failed(&pending, reason);
            }
        }
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::CreateLink => self.request(ClientMsg::CreateLink, Pending::CreateLink, false),
            Command::JoinLink { link } => self.join_link(link),
            Command::SendText {
                peer,
                msg_id,
                text,
                reply_to,
            } => {
                self.send_text(peer, msg_id, text, reply_to);
            }
            Command::SendFile {
                peer,
                msg_id,
                file_id,
                name,
                mime,
                size,
                kind,
                inline,
            } => self.send_file(files::NewFile {
                peer,
                msg_id,
                file_id,
                name,
                mime,
                size,
                kind,
                inline,
            }),
            Command::AcceptFile { file_id } => self.accept_file(&file_id),
            Command::CancelTransfer { file_id } => self.cancel_transfer(&file_id),
            Command::MarkRead { peer, ids } => self.mark_read(peer, &ids),
            Command::FetchInbox => self.fetch_inbox(),
            Command::RemovePeer { peer } => self.remove_peer(&peer),
            Command::RenamePeer { peer, alias } => self.rename_peer(&peer, alias.as_deref()),
            Command::DiscardOutgoing { msg_id } => self.discard_outgoing(&msg_id),
            Command::AcceptContact { peer } => self.accept_contact(&peer),
            Command::BlockPeer { peer } => self.set_blocked(&peer, true),
            Command::UnblockPeer { peer } => self.set_blocked(&peer, false),
            Command::SetProfileName { name } => self.set_profile_name(name.as_deref()),
            Command::SetAnonymity { require_onion } => self.anon.set_require_onion(require_onion),
            Command::EnablePush => self.enable_push(),
            Command::RegisterPush {
                endpoint,
                p256dh,
                auth,
            } => self.register_push(Subscription {
                endpoint,
                p256dh,
                auth,
            }),
            Command::DisablePush => self.disable_push(),
        }
    }

    fn on_tick(&mut self) {
        self.anon.expire(self.now);
        let expired: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= self.now)
            .map(|(&id, _)| id)
            .collect();
        for id in expired {
            if let Some((p, _)) = self.pending.remove(&id) {
                self.fail_request(p, FailReason::Timeout);
            }
        }

        if self.conn == Conn::Offline {
            return;
        }
        if self.now.saturating_sub(self.last_rx) >= DEAD_AFTER_MS {
            self.effects.push(Effect::Disconnect { reconnect: true });
            self.on_disconnected();
            return;
        }
        if self.conn != Conn::Ready {
            return;
        }
        if self.now.saturating_sub(self.last_ping) >= PING_INTERVAL_MS {
            self.last_ping = self.now;
            self.transmit(0, ClientMsg::Ping);
        }
        if self.now.saturating_sub(self.last_inbox_fetch) >= INBOX_POLL_MS {
            self.fetch_inbox();
        }
        if self.prekeys.rotate_if_due(self.now, &mut self.rng) {
            self.persist_prekeys();
            self.publish_keys(false);
        }
        self.restart_unanswered_inits();
        self.retransmit_expired();
        self.flush_outbox();
    }

    /// A known contact's message did not decrypt: the two sessions drifted
    /// apart (state lost on one side), and every later message would fail
    /// too. Starts a fresh one from their published keys, keeping the
    /// contact.
    pub(super) fn repair_session(&mut self, peer: PeerId) {
        let settled = self
            .peers
            .get(&peer)
            .is_some_and(|p| !p.request && !p.is_unconfirmed_initiator());
        if settled {
            self.refetch_keys(peer);
        }
    }

    /// Our first message reached the contact's device, yet nothing came
    /// back: it no longer has the prekeys we used (replaced after we fetched
    /// them), so it cannot answer. Starts again from its current keys.
    fn restart_unanswered_inits(&mut self) {
        let peers = &self.peers;
        self.init_heard
            .retain(|peer, _| peers.get(peer).is_some_and(Peer::is_unconfirmed_initiator));
        let due: Vec<PeerId> = self
            .init_heard
            .iter()
            .filter(|(_, at)| self.now.saturating_sub(**at) >= INIT_TIMEOUT_MS)
            .map(|(peer, _)| *peer)
            .collect();
        for peer in due {
            self.init_heard.remove(&peer);
            self.refetch_keys(peer);
        }
    }

    /// Fetches a contact's keys for a fresh session; at most every ten
    /// minutes per contact.
    fn refetch_keys(&mut self, peer: PeerId) {
        let blocked = self.peers.get(&peer).is_none_or(|p| p.blocked);
        let recent = self
            .last_repair
            .get(&peer)
            .is_some_and(|at| self.now.saturating_sub(*at) < REPAIR_INTERVAL_MS);
        if blocked || recent || self.conn != Conn::Ready {
            return;
        }
        self.last_repair.insert(peer, self.now);
        self.request(
            ClientMsg::FetchKeys { peer },
            Pending::Repair { peer },
            false,
        );
    }

    /// An initiator used prekeys this device does not have: the server's set
    /// is not ours (saved state lost after publishing, or two clients on one
    /// data). Publishing a fresh set lets the next contact through. At most
    /// once an hour, so strangers cannot churn our keys.
    pub(super) fn resync_prekeys(&mut self) {
        let recent = self
            .last_resync
            .is_some_and(|at| self.now.saturating_sub(at) < RESYNC_INTERVAL_MS);
        if recent || self.conn != Conn::Ready {
            return;
        }
        self.last_resync = Some(self.now);
        self.publish_keys(true);
    }

    fn publish_keys(&mut self, batch: bool) {
        let opks = if batch {
            let opks = self.prekeys.new_batch(&mut self.rng);
            self.persist_prekeys();
            opks
        } else {
            Vec::new()
        };
        let base = Bytes::from(self.prekeys.base_bundle(&self.identity));
        self.request(
            ClientMsg::PublishKeys {
                base,
                opks,
                replace_opks: batch,
            },
            Pending::Publish { batch },
            false,
        );
    }

    fn fetch_inbox(&mut self) {
        if self.conn != Conn::Ready
            || self
                .pending
                .values()
                .any(|(p, _)| matches!(p, Pending::InboxFetch))
            || !matches!(
                self.anon.readiness(self.now),
                Readiness::Onion | Readiness::Session
            )
        {
            return;
        }
        self.last_inbox_fetch = self.now;
        self.request(
            ClientMsg::InboxFetch {
                secret: *self.inbox_secret,
            },
            Pending::InboxFetch,
            true,
        );
    }

    fn on_inbox_batch(&mut self, claim: [u8; 16], items: Vec<Bytes>) {
        let full = items.len() >= cypher_wire::MAX_INBOX_BATCH;
        for item in items {
            let Ok(plain) = cypher_crypto::sealed::open(&self.identity.dh_secret, &item) else {
                continue;
            };
            let Some((from, body)) = plain.split_first_chunk::<32>() else {
                continue;
            };
            self.on_relay(PeerId(*from), &Bytes::copy_from_slice(body), true);
        }
        self.request(
            ClientMsg::InboxAck {
                secret: *self.inbox_secret,
                claim,
            },
            Pending::InboxAck,
            true,
        );
        if full {
            self.last_inbox_fetch = 0;
        }
    }

    fn join_link(&mut self, link: String) {
        match ShareLink::parse(&link) {
            Some(share) => self.request(
                ClientMsg::ResolveLink {
                    link: share.link().clone(),
                },
                Pending::Resolve { link: share },
                false,
            ),
            None => self.emit(Event::JoinFailed {
                link,
                reason: FailReason::InvalidLink,
            }),
        }
    }

    /// The server says who made the link; only its fingerprint, which the
    /// server never saw, says whether to believe it.
    fn on_link_resolved(&mut self, share: &ShareLink, peer: PeerId) {
        let reason = if peer == self.peer_id {
            Some(FailReason::SelfLink)
        } else if !share.is_host(&peer) {
            Some(FailReason::KeyMismatch)
        } else {
            None
        };
        let link = share.to_string();
        if let Some(reason) = reason {
            self.emit(Event::JoinFailed { link, reason });
            return;
        }
        self.request(
            ClientMsg::FetchKeys { peer },
            Pending::FetchKeys { link, peer },
            false,
        );
    }

    /// Sends a request and tracks it for correlation and timeout. Anonymous
    /// requests go through the anonymity layer instead of the session.
    fn request(&mut self, msg: ClientMsg, pending: Pending, anonymous: bool) {
        if self.conn != Conn::Ready {
            self.fail_request(pending, FailReason::Offline);
            return;
        }
        let req_id = self.alloc_req();
        let deadline = self.now + REQUEST_TIMEOUT_MS;
        let frame = Frame::new(req_id, msg).encode();
        let effect = match (anonymous, self.anon.readiness(self.now)) {
            (false, _) | (true, Readiness::Session) => Effect::Transmit(frame),
            (true, Readiness::Onion) => {
                if let Some(blob) = self.anon.seal(&frame, self.now, deadline, &mut self.rng) {
                    Effect::Anonymous(blob)
                } else {
                    self.fail_request(pending, FailReason::Offline);
                    return;
                }
            }
            (true, Readiness::Wait | Readiness::Unavailable) => {
                self.fail_request(pending, FailReason::Offline);
                return;
            }
        };
        self.pending.insert(req_id, (pending, deadline));
        self.effects.push(effect);
    }

    fn alloc_req(&mut self) -> u32 {
        self.next_req = self.next_req.wrapping_add(1).max(1);
        self.next_req
    }

    fn transmit(&mut self, req_id: u32, msg: ClientMsg) {
        self.effects
            .push(Effect::Transmit(Frame::new(req_id, msg).encode()));
    }

    fn emit(&mut self, event: Event) {
        self.effects.push(Effect::Emit(event));
    }

    fn persist(&mut self, op: StoreOp) {
        self.effects.push(Effect::Persist(op));
    }

    fn persist_prekeys(&mut self) {
        let op = self.vault.put(
            Table::Meta,
            META_PREKEYS.to_vec(),
            &self.prekeys.to_record(),
            &mut self.rng,
        );
        self.persist(op);
    }

    fn persist_peer(&mut self, peer: &PeerId) {
        if let Some(p) = self.peers.get(peer) {
            let op = self
                .vault
                .put(Table::Peers, peer.to_vec(), &p.to_record(), &mut self.rng);
            self.effects.push(Effect::Persist(op));
        }
    }
}

/// Records `restore` could not read. One damaged row must not lock the user
/// out of everything else, so it is left in place, skipped and reported.
/// Data from a newer release is different: running on it could overwrite
/// state that release depends on, so it stops the restore instead.
#[derive(Default)]
struct Skipped(usize);

impl Skipped {
    fn open<T: Record>(
        &mut self,
        vault: &Vault,
        table: Table,
        key: &[u8],
        value: &[u8],
    ) -> Result<Option<T>, CoreError> {
        match vault.open(table, key, value) {
            Ok(record) => Ok(Some(record)),
            Err(CoreError::NewerStorage) => Err(CoreError::NewerStorage),
            Err(_) => {
                self.0 = self.0.saturating_add(1);
                Ok(None)
            }
        }
    }
}

fn load_peers(
    vault: &Vault,
    rows: &Rows,
    skipped: &mut Skipped,
) -> Result<HashMap<PeerId, Peer>, CoreError> {
    let mut peers = HashMap::with_capacity(rows.len());
    for (k, v) in rows {
        let record = skipped.open::<PeerRecord>(vault, Table::Peers, k, v)?;
        match (PeerId::from_bytes(k), record.map(|r| Peer::from_record(&r))) {
            (Some(id), Some(Ok(peer))) => {
                peers.insert(id, peer);
            }
            (_, None) => {}
            _ => skipped.0 = skipped.0.saturating_add(1),
        }
    }
    Ok(peers)
}

fn load_outbox(
    vault: &Vault,
    rows: &Rows,
    skipped: &mut Skipped,
) -> Result<BTreeMap<MsgId, OutboxItem>, CoreError> {
    let mut outbox = BTreeMap::new();
    for (k, v) in rows {
        if let Some(item) = skipped.open::<OutboxItem>(vault, Table::Outbox, k, v)? {
            outbox.insert(item.msg_id, item);
        }
    }
    Ok(outbox)
}

type Transfers = (HashMap<FileId, Outgoing>, HashMap<FileId, Incoming>);

fn load_meta<T: Record>(
    vault: &Vault,
    meta: &Rows,
    key: &[u8],
    skipped: &mut Skipped,
) -> Result<Option<T>, CoreError> {
    Ok(meta
        .iter()
        .find(|(k, _)| k == key)
        .map(|(k, v)| skipped.open::<T>(vault, Table::Meta, k, v))
        .transpose()?
        .flatten())
}

fn load_profile(
    vault: &Vault,
    meta: &Rows,
    skipped: &mut Skipped,
) -> Result<Option<String>, CoreError> {
    Ok(meta
        .iter()
        .find(|(k, _)| k == META_PROFILE)
        .map(|(k, v)| skipped.open::<ProfileRecord>(vault, Table::Meta, k, v))
        .transpose()?
        .flatten()
        .and_then(|record| record.name))
}

fn load_transfers(
    vault: &Vault,
    rows: &Rows,
    skipped: &mut Skipped,
) -> Result<Transfers, CoreError> {
    let (mut outgoing, mut incoming) = (HashMap::new(), HashMap::new());
    for (k, v) in rows {
        let Some(rec) = skipped.open::<TransferRecord>(vault, Table::Transfers, k, v)? else {
            continue;
        };
        let id = rec.desc.file_id;
        if rec.outgoing {
            outgoing.insert(id, Outgoing::from_record(rec));
        } else {
            incoming.insert(id, Incoming::from_record(rec));
        }
    }
    Ok((outgoing, incoming))
}

#[derive(Default)]
struct RecentIds {
    set: HashSet<MsgId>,
    order: VecDeque<MsgId>,
}

impl RecentIds {
    fn contains(&self, id: &MsgId) -> bool {
        self.set.contains(id)
    }

    /// Returns false when `id` was already seen.
    fn insert(&mut self, id: MsgId) -> bool {
        if !self.set.insert(id) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > RECENT_IDS
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
}
