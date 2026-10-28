mod anon;
mod devices;
mod files;
mod held;
mod history;
mod inbox;
mod messaging;
mod outbox;
mod push;
mod servers;
mod sync;

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use bytes::Bytes;
use cypher_crypto::{DeviceList, IdentityKeyPair, IdentitySeed};
use cypher_types::{Addr, DeviceId, FileId, LinkId, MsgId, PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, ErrorCode, Frame, PROTOCOL_VERSION, ServerMsg};
use rand_core::CryptoRngCore;

use crate::CoreError;
use crate::api::{Command, CommandId, Effect, Event, FailReason, Input};
use crate::envelope::SyncBody;
use crate::link::LinkOffer;
use crate::peer::{OwnLinks, Peer, PeerRecord, ProfileRecord, Session, SessionRecord};
use crate::prekeys::{OPK_LOW_WATER, Prekeys, PrekeysRecord};
use crate::share::ShareLink;
use crate::store::{
    META_DEVICES, META_HISTORY, META_INBOX, META_LINKS, META_PREKEYS, META_PROFILE, META_SERVERS,
    Record, StoreOp, Table, Vault, session_addr, session_key,
};
use crate::transfer::{Incoming, Outgoing, TransferRecord};

use anon::{Anon, Opened, Readiness};
use devices::{Devices, DevicesFor, OwnDevices};
use held::HeldSent;
use history::History;
use inbox::InboxEpochs;
use messaging::Joining;
use outbox::OutboxItem;
use push::{PushState, Subscription};
use servers::ServerKeys;

/// Anonymous requests that wait for the relay at most; more fail at once.
const MAX_DEFERRED: usize = 64;
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
    CreateLink {
        id: CommandId,
    },
    Resolve {
        id: CommandId,
        link: ShareLink,
    },
    /// Keys of one device of the host of an invite we are joining by.
    FetchKeys {
        link: String,
        addr: Addr,
    },
    /// Keys for a session with one device of a contact: one it has none
    /// with yet, or one whose messages stopped decrypting.
    SessionKeys {
        addr: Addr,
    },
    FetchDevices {
        peer: PeerId,
        purpose: DevicesFor,
    },
    PublishDevices {
        list: DeviceList,
        /// A device this one just linked, listed by this publication.
        linked: Option<devices::Linked>,
    },
    /// The hand-over to a new device, for `LinkDevice` `id`.
    LinkSend {
        id: CommandId,
        offer: LinkOffer,
    },
    Publish {
        batch: bool,
    },
    Send {
        msg_id: MsgId,
        addr: Addr,
        body: Bytes,
    },
    /// The same message sent without our name, through the relay.
    SendSealed {
        msg_id: MsgId,
        addr: Addr,
        body: Bytes,
    },
    InboxPut {
        msg_id: MsgId,
        addr: Addr,
    },
    /// A read of the inbox of `epoch`; its last one, which closes it, if
    /// `close`.
    InboxFetch {
        epoch: u32,
        close: bool,
    },
    InboxAck {
        epoch: u32,
        close: bool,
    },
    Bootstrap,
    PushKey,
    /// The gateway took this device's access tokens.
    Access,
    PushRegister {
        epoch: u32,
    },
    PushUnregister,
}

/// What a server error means for the request it answers.
fn failure(code: ErrorCode) -> FailReason {
    match code {
        ErrorCode::NotFound => FailReason::NotFound,
        ErrorCode::Unauthorized => FailReason::Unauthorized,
        ErrorCode::Conflict => FailReason::Rejected,
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
    pub sessions: Rows,
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
    /// Kept to derive the inboxes of this identity's other devices.
    seed: IdentitySeed,
    identity: IdentityKeyPair,
    peer_id: PeerId,
    /// Which of the identity's devices this one is.
    device: DeviceId,
    vault: Vault,
    prekeys: Prekeys,
    peers: HashMap<PeerId, Peer>,
    /// One per device of a contact we talk to.
    sessions: HashMap<Addr, Session>,
    outbox: BTreeMap<MsgId, OutboxItem>,
    // Maps the core walks to make effects are ordered: the same inputs make
    // the same effects in the same order.
    outgoing: BTreeMap<FileId, Outgoing>,
    incoming: BTreeMap<FileId, Incoming>,
    pending: BTreeMap<u32, (Pending, u64)>,
    /// Anonymous requests made before it was known how they can go: sent
    /// once the relay is up, or its wait is over.
    deferred: VecDeque<(ClientMsg, Pending)>,
    next_req: u32,
    conn: Conn,
    last_rx: u64,
    last_ping: u64,
    last_inbox_fetch: u64,
    /// When the inboxes beyond the last epoch were last read.
    last_inbox_sweep: Option<u64>,
    /// When keys were last republished because the server's were not ours.
    last_resync: Option<u64>,
    /// When a fresh session was last started with a device of a contact.
    last_repair: BTreeMap<Addr, u64>,
    /// When a message carrying our unanswered first one reached a
    /// contact's device.
    init_heard: BTreeMap<Addr, u64>,
    /// Invites being joined by, while the host's devices answer.
    joining: HashMap<PeerId, Joining>,
    devices: Devices,
    history: History,
    held: HeldSent,
    recent: RecentIds,
    progress_at: HashMap<FileId, u64>,
    /// Chunks read from disk for the devices waiting for them.
    reads: HashMap<(FileId, u32), Vec<DeviceId>>,
    anon: Anon,
    /// The address of the server this device connects to.
    server: String,
    server_keys: ServerKeys,
    inbox_epochs: InboxEpochs,
    /// A signing key a server showed that is not the one remembered for
    /// it, and that server's address: the key is trusted for it alone.
    offered_key: Option<(String, [u8; 32])>,
    push: PushState,
    /// The name this user goes by, sent to contacts with `Hello`.
    profile_name: Option<String>,
    /// Invites this user made and when; each admits one contact.
    own_links: Vec<(String, u64)>,
    effects: Vec<Effect>,
}

impl<R: CryptoRngCore> Core<R> {
    /// Builds the core for one device of the identity `seed` from previously
    /// persisted state. Returns persistence effects when fresh prekeys had to
    /// be generated.
    pub fn restore(
        seed: &IdentitySeed,
        device: DeviceId,
        snapshot: &Snapshot,
        now_ms: u64,
        mut rng: R,
    ) -> Result<(Self, Vec<Effect>), CoreError> {
        let identity = seed.derive_identity();
        let vault = Vault::new(seed.derive_storage_key());

        let mut skipped = Skipped::default();
        let (prekeys, fresh_prekeys) =
            load_prekeys(&vault, &snapshot.meta, now_ms, &mut rng, &mut skipped)?;

        let own = load_own(&vault, &snapshot.meta, &mut skipped)?;
        let (peers, sessions, legacy) =
            load_contacts(&vault, snapshot, identity.peer_id(), &mut skipped)?;
        let outbox = load_outbox(&vault, &snapshot.outbox, &mut skipped)?;
        let (outgoing, incoming) = load_transfers(&vault, &snapshot.transfers, &mut skipped)?;
        let inboxes_new = own.inbox.is_none();

        let mut core = Self {
            rng,
            now: now_ms,
            peer_id: identity.peer_id(),
            device,
            seed: IdentitySeed(*seed.as_bytes()),
            identity,
            vault,
            prekeys,
            peers,
            sessions,
            outbox,
            outgoing,
            incoming,
            pending: BTreeMap::new(),
            deferred: VecDeque::new(),
            next_req: 0,
            conn: Conn::Offline,
            last_rx: now_ms,
            last_ping: now_ms,
            last_inbox_fetch: 0,
            last_inbox_sweep: None,
            last_resync: None,
            last_repair: BTreeMap::new(),
            init_heard: BTreeMap::new(),
            joining: HashMap::new(),
            devices: own.devices,
            history: own.history,
            held: own.held,
            recent: RecentIds::default(),
            progress_at: HashMap::new(),
            reads: HashMap::new(),
            anon: Anon::default(),
            server: String::new(),
            server_keys: own.servers,
            inbox_epochs: own.inbox.unwrap_or_default(),
            offered_key: None,
            push: PushState::default(),
            profile_name: own.profile_name,
            own_links: own.links,
            effects: Vec::new(),
        };
        core.adopt_legacy_sessions(legacy, &mut skipped);
        if core.prekeys.take_unseeded() {
            core.seed_accepted_inits();
        }
        core.pick_up(&skipped, fresh_prekeys, inboxes_new);
        let effects = std::mem::take(&mut core.effects);
        Ok((core, effects))
    }

    /// Prekeys saved before the inits accepted were remembered with them:
    /// the inits the sessions still remember are taken as accepted, so a
    /// copy of one stays refused once its session forgets it.
    fn seed_accepted_inits(&mut self) {
        let mut ephemerals: Vec<[u8; 32]> = self
            .sessions
            .values()
            .flat_map(|s| s.accepted_ephemerals.iter().copied())
            .collect();
        ephemerals.sort_unstable();
        self.prekeys.seed_inits(&ephemerals);
        self.persist_prekeys();
    }

    /// What a restored core stores, tells and asks for again: state it
    /// just made up, offers still waiting, history work the driver did not
    /// finish, records it could not read.
    fn pick_up(&mut self, skipped: &Skipped, fresh_prekeys: bool, fresh_inboxes: bool) {
        if fresh_prekeys {
            self.persist_prekeys();
        }
        if fresh_inboxes {
            self.start_inbox_epochs();
        }
        self.announce_pending_offers();
        self.resume_history();
        if skipped.0 > 0 {
            self.emit(Event::Warning {
                reason: FailReason::Corrupted,
            });
        }
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
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
            Input::Frame(bytes) => self.on_frame(bytes, None),
            Input::AnonymousFrame(bytes) => match self.anon.open(&bytes) {
                Some(Opened::Reply(req_id, frame)) => self.on_frame(frame, Some(req_id)),
                Some(Opened::KeySet) => self.on_onion_ready(),
                None => {}
            },
            Input::AnonymousChannel { up } => {
                self.anon.set_relay_up(up);
                self.emit(Event::Onion { up });
                self.ask_key_set();
                self.send_deferred();
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
            Input::HistoryBuilt { file_id, size } => self.on_history_built(file_id, size),
            Input::HistoryImported { file_id, ok } => self.on_history_imported(file_id, ok),
            Input::ChunkUnavailable { file_id } => {
                self.fail_outgoing(&file_id, FailReason::SourceUnavailable);
            }
            Input::Tick => self.on_tick(),
        }
        self.place_held();
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
                device: self.device,
            },
        );
    }

    fn on_disconnected(&mut self) {
        if self.conn == Conn::Offline {
            return;
        }
        self.conn = Conn::Offline;
        self.anon.on_disconnected();
        let pending = std::mem::take(&mut self.pending)
            .into_values()
            .map(|(p, _)| p);
        let deferred: Vec<_> = self.deferred.drain(..).map(|(_, p)| p).collect();
        for p in pending.chain(deferred) {
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
        self.tell_access();
        self.last_inbox_fetch = 0;
        self.check_own_devices();
        self.flush_outbox();
        self.resume_transfers();
    }

    /// A frame from the gateway session, or (`onion`) the reply through the
    /// relay to that request, which may answer no other.
    fn on_frame(&mut self, bytes: Bytes, onion: Option<u32>) {
        let session = onion.is_none();
        let Ok(Frame { req_id, msg }) = Frame::<ServerMsg>::decode(bytes) else {
            if session {
                self.effects.push(Effect::Disconnect { reconnect: true });
            }
            return;
        };
        if onion.is_some_and(|expected| expected != req_id) {
            return;
        }
        if !session
            && !matches!(
                msg,
                ServerMsg::InboxBatch { .. }
                    | ServerMsg::Done
                    | ServerMsg::SendAck { .. }
                    | ServerMsg::PushKey { .. }
                    | ServerMsg::Keys { .. }
                    | ServerMsg::Devices { .. }
                    | ServerMsg::LinkResolved { .. }
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
            ServerMsg::Recv { from, device, body } => {
                self.on_relay(Addr::new(from, device), &body, false);
            }
            // Sealed like an inbox item: who sent it is inside.
            ServerMsg::RecvSealed { item } => self.on_inbox_item(&item),
            msg @ (ServerMsg::SendAck { .. }
            | ServerMsg::Keys { .. }
            | ServerMsg::KeysAck { .. }
            | ServerMsg::Devices { .. }
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
            | ErrorCode::Conflict
            | ErrorCode::Internal => {}
        }
    }

    fn on_challenge(&mut self, nonce: &[u8; 32]) {
        if self.conn != Conn::Authenticating {
            return;
        }
        let mut signed = Vec::with_capacity(SESSION_AUTH_CONTEXT.len() + 36);
        signed.extend_from_slice(SESSION_AUTH_CONTEXT);
        signed.extend_from_slice(nonce);
        signed.extend_from_slice(&self.device.0.to_le_bytes());
        let signature = self.identity.sign(&signed).to_bytes();
        self.transmit(0, ClientMsg::Auth { signature });
    }

    fn on_response(&mut self, pending: Pending, msg: ServerMsg) {
        if let (Pending::SendSealed { msg_id, addr, .. }, ServerMsg::Error { code }) =
            (&pending, &msg)
        {
            self.on_sealed_refused(*msg_id, *addr, *code);
        } else if let ServerMsg::Error { code } = msg {
            self.fail_request(pending, failure(code));
        } else if matches!(
            pending,
            Pending::PushKey | Pending::PushRegister { .. } | Pending::PushUnregister
        ) {
            self.on_push_response(&pending, &msg);
        } else {
            self.on_success(pending, msg);
        }
    }

    fn on_success(&mut self, pending: Pending, msg: ServerMsg) {
        match (pending, msg) {
            (Pending::CreateLink { id }, ServerMsg::LinkCreated { link }) => {
                self.on_link_created(id, link);
            }
            (Pending::Resolve { id, link }, ServerMsg::LinkResolved { peer }) => {
                self.on_link_resolved(id, &link, peer);
            }
            (Pending::FetchKeys { link, addr }, ServerMsg::Keys { base, opk }) => {
                self.on_keys(&link, addr, &base, opk);
            }
            (Pending::SessionKeys { addr }, ServerMsg::Keys { base, opk }) => {
                self.on_session_keys(addr, &base, opk);
            }
            (Pending::FetchDevices { peer, purpose }, ServerMsg::Devices { list }) => {
                self.on_devices(peer, purpose, Some(&list));
            }
            (Pending::PublishDevices { list, linked }, ServerMsg::Done) => {
                self.on_own_published(list, linked);
            }
            (Pending::LinkSend { id, offer }, ServerMsg::SendAck { status }) => {
                self.on_link_sent(id, offer, status);
            }
            (Pending::Publish { batch }, ServerMsg::KeysAck { opks_left }) => {
                if !batch && opks_left < OPK_LOW_WATER {
                    self.publish_keys(true);
                }
            }
            (
                Pending::Send { msg_id, addr, body } | Pending::SendSealed { msg_id, addr, body },
                ServerMsg::SendAck { status },
            ) => {
                self.on_send_ack(msg_id, addr, &body, status);
            }
            (Pending::InboxPut { msg_id, addr }, ServerMsg::Done) => {
                self.on_inbox_queued(msg_id, addr.device);
            }
            (Pending::InboxFetch { epoch, close }, ServerMsg::InboxBatch { claim, items }) => {
                self.on_inbox_batch(epoch, close, claim, items);
            }
            (Pending::InboxAck { close, .. }, ServerMsg::Done) => {
                if close {
                    self.close_old_inbox();
                }
            }
            (
                Pending::Bootstrap,
                ServerMsg::BootstrapInfo {
                    relay_addr,
                    signing_key,
                    ..
                },
            ) => self.on_bootstrap(relay_addr, signing_key),
            (pending, _) => self.fail_request(pending, FailReason::ServerError),
        }
    }

    /// An invite made here: kept, told to this user's other devices, shown.
    fn on_link_created(&mut self, id: CommandId, link: LinkId) {
        self.remember_link(link.as_str(), self.now);
        self.sync(SyncBody::Link {
            code: link.as_str().to_owned(),
            at_ms: self.now,
        });
        self.emit(Event::LinkCreated {
            id,
            link: ShareLink::new(link, &self.peer_id).to_string(),
        });
    }

    /// Where the relay is and what to seal onion requests to.
    fn on_bootstrap(&mut self, relay_addr: String, signing_key: [u8; 32]) {
        let relay = if relay_addr.is_empty() {
            None
        } else {
            self.checked_server_key(signing_key)
        };
        self.anon.on_bootstrap(relay, self.now);
        self.ask_key_set();
        self.send_deferred();
        self.fetch_inbox();
        self.push_step();
        self.emit(Event::Bootstrap { relay_addr });
    }

    /// Asks the relay for the server's onion keys, if they are needed.
    fn ask_key_set(&mut self) {
        let deadline = self.now.saturating_add(REQUEST_TIMEOUT_MS);
        if let Some(frame) = self.anon.key_set_request(self.now, deadline) {
            self.effects.push(Effect::Anonymous(frame));
        }
    }

    /// The server's onion keys arrived: what waited for them goes now.
    fn on_onion_ready(&mut self) {
        self.send_deferred();
        self.flush_outbox();
        self.fetch_inbox();
        self.renew_push();
    }

    fn fail_request(&mut self, pending: Pending, reason: FailReason) {
        match pending {
            Pending::Resolve { id, link } => self.emit(Event::JoinFailed {
                id,
                link: link.to_string(),
                reason,
            }),
            Pending::FetchKeys { addr, .. } => self.on_join_keys_failed(addr.peer, reason),
            Pending::FetchDevices { peer, purpose } => match (purpose, reason) {
                (purpose, FailReason::NotFound) => self.on_devices(peer, purpose, None),
                (DevicesFor::Join, reason) => self.join_failed(peer, reason),
                (DevicesFor::Own | DevicesFor::Refresh, _) => {}
            },
            Pending::PublishDevices { linked, .. } => {
                if let Some(linked) = linked {
                    self.emit(Event::LinkFailed {
                        id: Some(linked.id),
                        reason,
                    });
                } else if reason == FailReason::Rejected {
                    self.on_own_conflict();
                }
            }
            Pending::LinkSend { id, .. } => self.emit(Event::LinkFailed {
                id: Some(id),
                reason,
            }),
            Pending::SessionKeys { addr } => {
                if reason == FailReason::NotFound {
                    self.on_keys_missing(addr);
                }
            }
            Pending::Bootstrap => {
                self.anon.on_bootstrap(None, self.now);
                self.send_deferred();
                self.fetch_inbox();
            }
            Pending::CreateLink { id } => self.emit(Event::CreateLinkFailed { id, reason }),
            Pending::InboxFetch { close: false, .. } => self.emit(Event::Warning { reason }),
            Pending::InboxPut { msg_id, addr } if reason == FailReason::NotFound => {
                self.on_inbox_gone(msg_id, addr);
            }
            Pending::Send { msg_id, addr, .. }
            | Pending::SendSealed { msg_id, addr, .. }
            | Pending::InboxPut { msg_id, addr } => {
                self.on_send_failed(msg_id, addr.device);
            }
            // Closing is tried again on the next tick.
            Pending::Publish { .. }
            | Pending::Access
            | Pending::InboxAck { .. }
            | Pending::InboxFetch { close: true, .. } => {}
            Pending::PushKey | Pending::PushRegister { .. } | Pending::PushUnregister => {
                self.on_push_failed(&pending, reason);
            }
        }
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::CreateLink { id } => self.create_link(id),
            Command::JoinLink { id, link } => self.join_link(id, link),
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
            Command::RemovePeer { peer } => self.forget_contact(peer),
            Command::RenamePeer { peer, alias } => self.rename_contact(peer, alias.as_deref()),
            Command::DiscardOutgoing { msg_id } => self.discard_outgoing(&msg_id),
            Command::AcceptContact { peer } => self.accept_request(peer),
            Command::BlockPeer { peer } => self.block_contact(peer, true),
            Command::UnblockPeer { peer } => self.block_contact(peer, false),
            Command::SetProfileName { name } => self.set_profile_name(name.as_deref()),
            Command::SetAnonymity { require_onion } => self.anon.set_require_onion(require_onion),
            Command::SetServer { addr } => self.set_server(addr),
            Command::TrustServerKey => self.trust_server_key(),
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
            Command::LinkDevice { id, offer, history } => self.link_device(id, &offer, history),
            Command::CancelHistory => self.cancel_history(),
            Command::UnlinkDevice { device } => self.unlink_device(DeviceId(device)),
            Command::ReplaceDevice { device } => self.replace_device(DeviceId(device)),
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
        self.ask_key_set();
        self.send_deferred();
        if self.now.saturating_sub(self.last_ping) >= PING_INTERVAL_MS {
            self.last_ping = self.now;
            self.transmit(0, ClientMsg::Ping);
        }
        if self.now.saturating_sub(self.last_inbox_fetch) >= INBOX_POLL_MS {
            self.fetch_inbox();
        }
        self.turn_inbox_epoch();
        if self.prekeys.rotate_if_due(self.now, &mut self.rng) {
            self.persist_prekeys();
            self.publish_keys(false);
        }
        self.restart_unanswered_inits();
        self.check_own_devices();
        self.retransmit_expired();
        self.flush_outbox();
    }

    /// A known contact's message did not decrypt: the two sessions drifted
    /// apart (state lost on one side), and every later message would fail
    /// too. Starts a fresh one from their published keys, keeping the
    /// contact.
    pub(super) fn repair_session(&mut self, addr: Addr) {
        let known =
            addr.peer == self.peer_id || self.peers.get(&addr.peer).is_some_and(|p| !p.request);
        let settled = known
            && !self
                .sessions
                .get(&addr)
                .is_some_and(Session::is_unconfirmed_initiator);
        if settled {
            self.open_session(addr);
        }
    }

    /// Our first message reached the contact's device, yet nothing came
    /// back: it no longer has the prekeys we used (replaced after we fetched
    /// them), so it cannot answer. Starts again from its current keys.
    fn restart_unanswered_inits(&mut self) {
        let sessions = &self.sessions;
        self.init_heard.retain(|addr, _| {
            sessions
                .get(addr)
                .is_some_and(Session::is_unconfirmed_initiator)
        });
        let due: Vec<Addr> = self
            .init_heard
            .iter()
            .filter(|(_, at)| self.now.saturating_sub(**at) >= INIT_TIMEOUT_MS)
            .map(|(addr, _)| *addr)
            .collect();
        for addr in due {
            self.init_heard.remove(&addr);
            self.open_session(addr);
        }
    }

    /// Fetches the keys of one device of a contact for a fresh session with
    /// it; at most every ten minutes per device.
    pub(super) fn open_session(&mut self, addr: Addr) {
        let blocked = if addr.peer == self.peer_id {
            addr.device == self.device
        } else {
            self.peers.get(&addr.peer).is_none_or(|p| p.blocked)
        };
        let recent = self
            .last_repair
            .get(&addr)
            .is_some_and(|at| self.now.saturating_sub(*at) < REPAIR_INTERVAL_MS);
        if blocked || recent || self.conn != Conn::Ready {
            return;
        }
        self.last_repair.insert(addr, self.now);
        self.request(
            ClientMsg::FetchKeys {
                peer: addr.peer,
                device: addr.device,
            },
            Pending::SessionKeys { addr },
            true,
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
        let base = Bytes::from(self.prekeys.base_bundle(&self.identity, self.device));
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

    fn create_link(&mut self, id: CommandId) {
        self.request(ClientMsg::CreateLink, Pending::CreateLink { id }, false);
    }

    fn join_link(&mut self, id: CommandId, link: String) {
        match ShareLink::parse(&link) {
            Some(share) => self.request(
                ClientMsg::ResolveLink {
                    link: share.link().clone(),
                },
                Pending::Resolve { id, link: share },
                true,
            ),
            None => self.emit(Event::JoinFailed {
                id,
                link,
                reason: FailReason::InvalidLink,
            }),
        }
    }

    /// The server says who made the link; only its fingerprint, which the
    /// server never saw, says whether to believe it.
    fn on_link_resolved(&mut self, id: CommandId, share: &ShareLink, peer: PeerId) {
        let reason = if peer == self.peer_id {
            Some(FailReason::SelfLink)
        } else if !share.is_host(&peer) {
            Some(FailReason::KeyMismatch)
        } else {
            None
        };
        let link = share.to_string();
        if let Some(reason) = reason {
            self.emit(Event::JoinFailed { id, link, reason });
            return;
        }
        if self.start_join(id, link, peer) {
            self.request(
                ClientMsg::FetchDevices { peer },
                Pending::FetchDevices {
                    peer,
                    purpose: DevicesFor::Join,
                },
                true,
            );
        }
    }

    /// Sends a request and tracks it for correlation and timeout. Anonymous
    /// requests go through the anonymity layer instead of the session.
    fn request(&mut self, msg: ClientMsg, pending: Pending, anonymous: bool) {
        if self.conn != Conn::Ready {
            self.fail_request(pending, FailReason::Offline);
            return;
        }
        let readiness = self.anon.readiness(self.now);
        if anonymous && readiness == Readiness::Wait {
            if self.deferred.len() < MAX_DEFERRED {
                self.deferred.push_back((msg, pending));
            } else {
                self.fail_request(pending, FailReason::Offline);
            }
            return;
        }
        let req_id = self.alloc_req();
        let deadline = self.now.saturating_add(REQUEST_TIMEOUT_MS);
        let frame = Frame::new(req_id, msg).encode();
        let effect = match (anonymous, readiness) {
            (false, _) | (true, Readiness::Session) => Effect::Transmit(frame),
            (true, Readiness::Onion) => {
                let sealed = self
                    .anon
                    .seal(req_id, &frame, self.now, deadline, &mut self.rng);
                if let Some(blob) = sealed {
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

    /// Sends what waited for the relay, once it is up or its wait is over:
    /// through the onion, over the session, or not at all.
    fn send_deferred(&mut self) {
        if self.deferred.is_empty() || self.anon.readiness(self.now) == Readiness::Wait {
            return;
        }
        for (msg, pending) in std::mem::take(&mut self.deferred) {
            self.request(msg, pending, true);
        }
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

    fn persist_session(&mut self, addr: Addr) {
        if let Some(s) = self.sessions.get(&addr) {
            let op = self.vault.put(
                Table::Sessions,
                session_key(addr),
                &s.to_record(),
                &mut self.rng,
            );
            self.effects.push(Effect::Persist(op));
        }
    }

    /// Contacts stored before devices held their one session inside: it
    /// becomes the session with their first device, in its own row, before
    /// the contact is stored again without it. Should the app stop between
    /// the two writes, the next start does it again.
    fn adopt_legacy_sessions(
        &mut self,
        legacy: Vec<(PeerId, SessionRecord)>,
        skipped: &mut Skipped,
    ) {
        for (peer, record) in legacy {
            let addr = Addr::new(peer, DeviceId::FIRST);
            if let Entry::Vacant(slot) = self.sessions.entry(addr) {
                match Session::from_record(&record) {
                    Ok(session) => {
                        slot.insert(session);
                        self.persist_session(addr);
                    }
                    Err(_) => skipped.0 = skipped.0.saturating_add(1),
                }
            }
            self.persist_peer(&peer);
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

/// Contacts, and the sessions that records from before devices still held.
type LoadedPeers = (HashMap<PeerId, Peer>, Vec<(PeerId, SessionRecord)>);

/// Contacts, the sessions with their devices and ours, and the sessions that
/// records from before devices still held.
type LoadedContacts = (
    HashMap<PeerId, Peer>,
    HashMap<Addr, Session>,
    Vec<(PeerId, SessionRecord)>,
);

fn load_contacts(
    vault: &Vault,
    snapshot: &Snapshot,
    own: PeerId,
    skipped: &mut Skipped,
) -> Result<LoadedContacts, CoreError> {
    let (peers, legacy) = load_peers(vault, &snapshot.peers, skipped)?;
    let sessions = load_sessions(vault, &snapshot.sessions, &peers, own, skipped)?;
    Ok((peers, sessions, legacy))
}

fn load_peers(vault: &Vault, rows: &Rows, skipped: &mut Skipped) -> Result<LoadedPeers, CoreError> {
    let mut peers = HashMap::with_capacity(rows.len());
    let mut legacy = Vec::new();
    for (k, v) in rows {
        let Some(mut record) = skipped.open::<PeerRecord>(vault, Table::Peers, k, v)? else {
            continue;
        };
        let Some(id) = PeerId::from_bytes(k) else {
            skipped.0 = skipped.0.saturating_add(1);
            continue;
        };
        if let Some(session) = record.legacy.take() {
            legacy.push((id, session));
        }
        peers.insert(id, Peer::from_record(&record));
    }
    Ok((peers, legacy))
}

/// Sessions with the devices of known contacts and our own other devices;
/// any other is left behind.
fn load_sessions(
    vault: &Vault,
    rows: &Rows,
    peers: &HashMap<PeerId, Peer>,
    own: PeerId,
    skipped: &mut Skipped,
) -> Result<HashMap<Addr, Session>, CoreError> {
    let mut sessions = HashMap::with_capacity(rows.len());
    for (k, v) in rows {
        let Some(record) = skipped.open::<SessionRecord>(vault, Table::Sessions, k, v)? else {
            continue;
        };
        match (session_addr(k), Session::from_record(&record)) {
            (Some(addr), Ok(session)) if addr.peer == own || peers.contains_key(&addr.peer) => {
                sessions.insert(addr, session);
            }
            (Some(_), Ok(_)) => {}
            _ => skipped.0 = skipped.0.saturating_add(1),
        }
    }
    Ok(sessions)
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

type Transfers = (BTreeMap<FileId, Outgoing>, BTreeMap<FileId, Incoming>);

/// Our saved prekeys, or a fresh set (`true`) when there are none.
fn load_prekeys(
    vault: &Vault,
    meta: &Rows,
    now_ms: u64,
    rng: &mut impl CryptoRngCore,
    skipped: &mut Skipped,
) -> Result<(Prekeys, bool), CoreError> {
    let stored = load_meta::<PrekeysRecord>(vault, meta, META_PREKEYS, skipped)?;
    Ok(match stored {
        Some(record) => (Prekeys::from_record(&record), false),
        None => (Prekeys::generate(now_ms, rng), true),
    })
}

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

/// What `meta` keeps of this user besides prekeys.
struct Own {
    profile_name: Option<String>,
    links: Vec<(String, u64)>,
    devices: Devices,
    history: History,
    held: HeldSent,
    servers: ServerKeys,
    inbox: Option<InboxEpochs>,
}

fn load_own(vault: &Vault, meta: &Rows, skipped: &mut Skipped) -> Result<Own, CoreError> {
    Ok(Own {
        profile_name: load_profile(vault, meta, skipped)?,
        links: load_meta::<OwnLinks>(vault, meta, META_LINKS, skipped)?
            .map(|record| record.links)
            .unwrap_or_default(),
        devices: Devices::from_record(load_meta::<OwnDevices>(vault, meta, META_DEVICES, skipped)?),
        history: load_meta::<History>(vault, meta, META_HISTORY, skipped)?.unwrap_or_default(),
        held: HeldSent::load(vault, meta, skipped)?,
        servers: load_meta::<ServerKeys>(vault, meta, META_SERVERS, skipped)?.unwrap_or_default(),
        inbox: load_meta::<InboxEpochs>(vault, meta, META_INBOX, skipped)?,
    })
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
    let (mut outgoing, mut incoming) = (BTreeMap::new(), BTreeMap::new());
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
