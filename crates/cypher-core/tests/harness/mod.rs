//! Deterministic in-memory world: a mock server with the gateway/signaling
//! semantics of wire v2 and client drivers around real `Core` instances.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash};

use bytes::Bytes;
use cypher_core::link::Provision;
use cypher_core::{Command, Core, Effect, Event, Input, Snapshot, StoreOp, Table, Vault, history};
use cypher_crypto::identity::verify_signature;
use cypher_crypto::inbox::{InboxOp, push_extra, verify_op};
use cypher_crypto::onion::{self, ReplyKey};
use cypher_crypto::onion_keys::{EpochKey, KeySet};
use cypher_crypto::{DeviceList, IdentityKeyPair, IdentitySeed, PrekeyBundle};
use cypher_types::{Addr, DeviceId, FileId, LinkId, MsgId, PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, DeliveryStatus, ErrorCode, Frame, InboxProof, ServerMsg, relay};
use rand::{Rng as _, SeedableRng};
use rand_chacha::ChaCha20Rng;
use x25519_dalek::{PublicKey, StaticSecret};

pub(crate) type Rng = ChaCha20Rng;

/// The one epoch the test server's onion key is published for.
const ONION_EPOCH: u32 = 1;

#[derive(Default, Clone)]
struct Keys {
    base: Bytes,
    opks: Vec<(u32, [u8; 32])>,
}

/// What the mock server does with a request a test wants to fail.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Fault {
    /// Never answered nor passed on, as if lost on the way.
    Ignore,
    /// Answered with this error.
    Error(ErrorCode),
}

/// Fails the next `times` requests that `matches` picks, over the session
/// or through the relay alike.
pub(crate) struct Rule {
    pub matches: Box<dyn Fn(&ClientMsg) -> bool>,
    pub fault: Fault,
    pub times: usize,
}

impl Rule {
    /// Fails every request `matches` picks.
    pub(crate) fn always(matches: impl Fn(&ClientMsg) -> bool + 'static, fault: Fault) -> Self {
        Self::times(usize::MAX, matches, fault)
    }

    /// Fails the next `times` requests `matches` picks.
    pub(crate) fn times(
        times: usize,
        matches: impl Fn(&ClientMsg) -> bool + 'static,
        fault: Fault,
    ) -> Self {
        Self {
            matches: Box::new(matches),
            fault,
            times,
        }
    }
}

/// One effect of a `handle` call, as a test looks at it later.
#[derive(Debug)]
pub(crate) enum Seen {
    /// A row written or deleted.
    Persist {
        table: u8,
        key: Vec<u8>,
    },
    Transmit(Bytes),
    Anonymous(Bytes),
    Emit(Event),
    ReadChunk(FileId),
    Other,
}

/// The effects one `handle` call (or a restore) returned, in order, and
/// the client's clock at the time.
#[derive(Debug)]
pub(crate) struct Batch {
    pub client: usize,
    pub at: u64,
    pub effects: Vec<Seen>,
}

#[derive(Default)]
pub(crate) struct Server {
    /// Requests to fail, first match wins.
    pub faults: Vec<Rule>,
    /// Every request served, in order: who sent it, its id and what it was.
    pub served: Vec<(usize, u32, ClientMsg)>,
    /// `FetchDevices` requests served.
    pub device_fetches: usize,
    online: HashMap<Addr, usize>,
    challenges: HashMap<usize, ([u8; 32], Addr)>,
    keys: HashMap<Addr, Keys>,
    /// Signed device lists, by identity.
    pub devices: HashMap<PeerId, DeviceList>,
    links: HashMap<String, PeerId>,
    inboxes: HashMap<[u8; 32], Vec<Bytes>>,
    /// Inboxes their owner has read, which take items, and those it closed.
    open_inboxes: HashSet<[u8; 32]>,
    pub closed_inboxes: HashSet<[u8; 32]>,
    claims: HashMap<[u8; 16], ([u8; 32], usize)>,
    /// Every `Recv` and `RecvSealed` delivered, for replay/tamper attacks.
    pub delivered: Vec<(usize, Bytes)>,
    pub drop_chunks_every: Option<usize>,
    /// Messages (not chunks) still to lose per sender: the server takes and
    /// acknowledges them, the recipient never sees them.
    pub lose_from: HashMap<PeerId, usize>,
    chunk_counter: usize,
    rng: Option<Rng>,
    pub session_inbox_ops: usize,
    pub onion_inbox_ops: usize,
    /// Invite lookups, device lists and keys asked for over the session and
    /// through the onion.
    pub session_intro_ops: usize,
    pub onion_intro_ops: usize,
    /// Push requests that came over the session instead of the relay.
    pub session_push_ops: usize,
    /// Push endpoint per inbox, and the inboxes signalled so far.
    pub pushes: HashMap<[u8; 32], String>,
    pub signals: Vec<[u8; 32]>,
    /// `FetchKeys` requests served.
    pub key_fetches: usize,
    /// Access-token hashes each connected device told.
    pub access: HashMap<Addr, Vec<[u8; 32]>>,
    /// Sealed sends asked for, and handed to a device online.
    pub sealed_sends: usize,
    pub sealed_delivered: usize,
    /// Signaling cannot reach any gateway for sealed deliveries.
    pub sealed_broken: bool,
    /// End-to-end messages (not file chunks) sent over a session, where the
    /// gateway sees who sends.
    pub named_messages: usize,
}

/// A driver's store: `(table, key)` to the sealed value.
pub(crate) type Kv = BTreeMap<(u8, Vec<u8>), Vec<u8>>;

pub(crate) struct Client {
    pub seed: [u8; 32],
    /// Which of its identity's devices this client is.
    pub device: DeviceId,
    pub core: Option<Core<Rng>>,
    /// A new device waiting to be linked, instead of a core.
    pub provision: Option<Provision>,
    pub kv: Kv,
    pub sources: HashMap<FileId, Vec<u8>>,
    pub sinks: HashMap<FileId, Vec<u8>>,
    pub closed: HashMap<FileId, bool>,
    /// The driver cannot build a history archive.
    pub history_fails: bool,
    /// The driver stops before it answers a history effect, as if the app
    /// were closed meanwhile.
    pub history_held: bool,
    /// What the driver tells the core each time it starts it, before it
    /// connects (the server's address, the anonymity setting).
    pub on_start: Vec<Command>,
    pub events: Vec<Event>,
    pub connected: bool,
    /// How far this client's clock runs ahead of the world's.
    pub ahead: u64,
    inputs: VecDeque<Input>,
    /// Request ids this core instance used: none may come twice.
    req_ids: HashSet<u32>,
}

impl Client {
    fn new(seed: [u8; 32], device: DeviceId, provision: Option<Provision>) -> Self {
        Self {
            seed,
            device,
            core: None,
            provision,
            kv: BTreeMap::new(),
            sources: HashMap::new(),
            sinks: HashMap::new(),
            closed: HashMap::new(),
            history_fails: false,
            history_held: false,
            on_start: Vec::new(),
            events: Vec::new(),
            connected: false,
            ahead: 0,
            inputs: VecDeque::new(),
            req_ids: HashSet::new(),
        }
    }
}

pub(crate) struct World {
    pub now: u64,
    pub server: Server,
    pub clients: Vec<Client>,
    pub relay_up: bool,
    wire: VecDeque<(usize, Bytes)>,
    onion_wire: VecDeque<(usize, Bytes)>,
    onion_secret: StaticSecret,
    /// The server's long-term key: it signs the onion keys.
    pub server_key: IdentityKeyPair,
    onion_ctx: Option<(usize, u64, ReplyKey)>,
    rng: Rng,
    /// A hash of every effect the cores made, in order, once set.
    pub trace: Option<DefaultHasher>,
    /// Every frame a client transmitted, in order.
    pub transmitted: Vec<(usize, Bytes)>,
    /// The driver of this client "crashes" right after its first `Send` to
    /// this device: `crash_rows` keeps only the rows persisted until then.
    pub crash_after_send: Option<(usize, Addr)>,
    pub crash_rows: Option<Kv>,
    /// Every batch of effects the cores made, once set.
    pub log: Option<Vec<Batch>>,
}

impl World {
    pub(crate) fn new(n: usize) -> Self {
        let mut rng = Rng::seed_from_u64(42);
        let mut world = Self {
            now: 1_000_000,
            server: Server {
                rng: Some(Rng::seed_from_u64(7)),
                ..Server::default()
            },
            clients: Vec::new(),
            relay_up: true,
            wire: VecDeque::new(),
            onion_wire: VecDeque::new(),
            onion_secret: StaticSecret::from([3u8; 32]),
            server_key: IdentitySeed([5; 32]).derive_identity(),
            onion_ctx: None,
            rng: Rng::seed_from_u64(1),
            trace: None,
            transmitted: Vec::new(),
            crash_after_send: None,
            crash_rows: None,
            log: None,
        };
        for _ in 0..n {
            let seed: [u8; 32] = rng.r#gen();
            world.clients.push(Client::new(seed, DeviceId::FIRST, None));
        }
        for i in 0..n {
            world.boot(i);
            world.connect(i);
        }
        world.run();
        world
    }

    /// Another device of client `owner`'s identity, online: the index of
    /// the new client.
    pub(crate) fn add_device(&mut self, owner: usize, device: u32) -> usize {
        let seed = self.clients[owner].seed;
        self.clients.push(Client::new(seed, DeviceId(device), None));
        let i = self.clients.len() - 1;
        self.boot(i);
        self.connect(i);
        self.run();
        i
    }

    /// A new device waiting to be linked, online: the index of the new
    /// client.
    pub(crate) fn provision(&mut self, name: &str) -> usize {
        let provision = Provision::new(name, self.now, &mut self.rng);
        let device = provision.offer().device;
        self.clients
            .push(Client::new([0; 32], device, Some(provision)));
        let i = self.clients.len() - 1;
        self.connect(i);
        self.run();
        i
    }

    /// The offer new device `i` shows.
    pub(crate) fn offer(&self, i: usize) -> String {
        self.clients[i]
            .provision
            .as_ref()
            .expect("a device waiting to be linked")
            .offer()
            .to_text()
    }

    /// New device `i` took the identity handed to it and starts as a device
    /// of it; false when nothing was handed over.
    pub(crate) fn finish_linking(&mut self, i: usize) -> bool {
        let Some(linked) = self.clients[i]
            .provision
            .as_mut()
            .and_then(Provision::take_linked)
        else {
            return false;
        };
        let c = &mut self.clients[i];
        c.seed = *linked.seed.as_bytes();
        c.device = linked.device;
        c.provision = None;
        self.server.online.retain(|_, idx| *idx != i);
        self.boot(i);
        self.connect(i);
        self.run();
        true
    }

    pub(crate) fn peer(&self, i: usize) -> PeerId {
        self.clients[i].core.as_ref().unwrap().peer_id()
    }

    /// What a driver reads from client `i`'s store at startup.
    pub(crate) fn snapshot(&self, i: usize) -> Snapshot {
        let c = &self.clients[i];
        let pick = |t: Table| -> Vec<(Vec<u8>, Vec<u8>)> {
            c.kv.iter()
                .filter(|((table, _), _)| *table == t as u8)
                .map(|((_, k), v)| (k.clone(), v.clone()))
                .collect()
        };
        Snapshot {
            meta: pick(Table::Meta),
            peers: pick(Table::Peers),
            sessions: pick(Table::Sessions),
            outbox: pick(Table::Outbox),
            transfers: pick(Table::Transfers),
        }
    }

    /// (Re)creates the core from the client's persisted key-value state.
    pub(crate) fn boot(&mut self, i: usize) {
        let snapshot = self.snapshot(i);
        let rng = Rng::seed_from_u64(self.rng.r#gen());
        let c = &mut self.clients[i];
        let (core, effects) =
            Core::restore(&IdentitySeed(c.seed), c.device, &snapshot, self.now, rng).unwrap();
        c.core = Some(core);
        c.connected = false;
        c.req_ids.clear();
        let start = c.on_start.iter().cloned().map(Input::Command);
        c.inputs.extend(start);
        self.record(i, self.now, &effects);
        self.apply(i, effects);
    }

    /// Keeps `effects` in the log, if one is kept.
    fn record(&mut self, client: usize, at: u64, effects: &[Effect]) {
        if let Some(log) = self.log.as_mut() {
            log.push(Batch {
                client,
                at,
                effects: effects.iter().map(seen).collect(),
            });
        }
    }

    /// Hands client `i` a command without running the world: what it
    /// does happens at the next [`World::run`], alongside what others do.
    pub(crate) fn queue(&mut self, i: usize, cmd: Command) {
        self.clients[i].inputs.push_back(Input::Command(cmd));
    }

    /// The server forgets the keys `addr` published, as if it never had.
    pub(crate) fn remove_keys(&mut self, addr: Addr) {
        self.server.keys.remove(&addr);
    }

    /// The request inside an onion blob a client handed its relay, as
    /// the relay would forward it; `None` for a key set request.
    pub(crate) fn open_anonymous(&self, blob: &[u8]) -> Option<Frame<ClientMsg>> {
        let (_, rest) = blob.split_at_checked(relay::CORR_LEN)?;
        let (tag, payload) = rest.split_first()?;
        if *tag != relay::TAG_ONION {
            return None;
        }
        let (_, sealed) = onion::split_epoch(payload).ok()?;
        let opened = onion::open_request(&self.onion_secret, sealed).ok()?;
        Frame::<ClientMsg>::decode(Bytes::from(opened.frame)).ok()
    }

    pub(crate) fn connect(&mut self, i: usize) {
        self.clients[i].connected = true;
        self.clients[i].inputs.push_back(Input::Connected);
        self.clients[i]
            .inputs
            .push_back(Input::AnonymousChannel { up: self.relay_up });
    }

    /// Client `i`'s relay channel goes up or down.
    pub(crate) fn set_relay(&mut self, i: usize, up: bool) {
        self.clients[i]
            .inputs
            .push_back(Input::AnonymousChannel { up });
        self.run();
    }

    pub(crate) fn disconnect(&mut self, i: usize) {
        self.server.online.retain(|_, idx| *idx != i);
        self.clients[i].connected = false;
        self.clients[i].inputs.push_back(Input::Disconnected);
        self.run();
    }

    /// Whether client `i` keeps a session with `peer`.
    pub(crate) fn has_session(&self, i: usize, peer: PeerId) -> bool {
        self.clients[i]
            .kv
            .contains_key(&(Table::Peers as u8, peer.to_vec()))
    }

    /// The name client `i` stored for `peer`, whose session must exist.
    pub(crate) fn alias(&self, i: usize, peer: PeerId) -> Option<String> {
        let c = &self.clients[i];
        let vault = Vault::new(IdentitySeed(c.seed).derive_storage_key());
        let sealed = &c.kv[&(Table::Peers as u8, peer.to_vec())];
        vault.open_contact(peer.as_bytes(), sealed).unwrap().alias
    }

    /// What client `i` knows of `peer`: names, request and block state.
    pub(crate) fn contact(&self, i: usize, peer: PeerId) -> cypher_core::ContactInfo {
        let c = &self.clients[i];
        let vault = Vault::new(IdentitySeed(c.seed).derive_storage_key());
        let sealed = &c.kv[&(Table::Peers as u8, peer.to_vec())];
        vault.open_contact(peer.as_bytes(), sealed).unwrap()
    }

    /// The name `peer` told client `i` they go by.
    pub(crate) fn contact_name(&self, i: usize, peer: PeerId) -> Option<String> {
        let c = &self.clients[i];
        let vault = Vault::new(IdentitySeed(c.seed).derive_storage_key());
        let sealed = &c.kv[&(Table::Peers as u8, peer.to_vec())];
        vault.open_contact(peer.as_bytes(), sealed).unwrap().name
    }

    pub(crate) fn restart(&mut self, i: usize) {
        self.server.online.retain(|_, idx| *idx != i);
        self.boot(i);
        self.connect(i);
        self.run();
    }

    pub(crate) fn command(&mut self, i: usize, cmd: Command) {
        self.clients[i].inputs.push_back(Input::Command(cmd));
        self.run();
    }

    pub(crate) fn advance(&mut self, ms: u64) {
        let step = 500;
        let mut left = ms;
        while left > 0 {
            let d = left.min(step);
            self.now += d;
            left -= d;
            for c in &mut self.clients {
                c.inputs.push_back(Input::Tick);
            }
            self.run();
        }
    }

    pub(crate) fn msg_id(&mut self) -> MsgId {
        MsgId(self.rng.r#gen())
    }

    pub(crate) fn file_id(&mut self) -> FileId {
        FileId(self.rng.r#gen())
    }

    pub(crate) fn create_link(&mut self, i: usize) -> String {
        self.command(i, Command::CreateLink);
        self.clients[i]
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::LinkCreated { link } => Some(link.clone()),
                _ => None,
            })
            .expect("link created")
    }

    /// The server takes `i`'s next `n` messages and loses them.
    pub(crate) fn lose_messages_from(&mut self, i: usize, n: usize) {
        let peer = self.peer(i);
        self.server.lose_from.insert(peer, n);
    }

    /// Stored messages of `i` with this id (a resent one is stored once).
    pub(crate) fn stored_copies(&self, i: usize, id: MsgId) -> usize {
        self.clients[i]
            .kv
            .keys()
            .filter(|(table, key)| *table == Table::Messages as u8 && key.ends_with(&id.0))
            .count()
    }

    /// Makes the server hand out one-time prekeys `i` never had, as when its
    /// saved state fell behind what it had published.
    pub(crate) fn foreign_opks(&mut self, i: usize) {
        let peer = self.peer(i);
        let entry = self
            .server
            .keys
            .get_mut(&Addr::new(peer, DeviceId::FIRST))
            .expect("keys published");
        entry.opks = (9000..9010)
            .map(|id| {
                let secret = StaticSecret::from(self.rng.r#gen::<[u8; 32]>());
                (id, PublicKey::from(&secret).to_bytes())
            })
            .collect();
    }

    /// The server has none of client `i`'s one-time prekeys left: whoever
    /// fetches its keys gets the signed prekey alone.
    pub(crate) fn exhaust_opks(&mut self, i: usize) {
        let addr = Addr::new(self.peer(i), self.clients[i].device);
        if let Some(keys) = self.server.keys.get_mut(&addr) {
            keys.opks.clear();
        }
    }

    /// Makes the server answer `FetchKeys` for `of` with the bundle `with`
    /// published (validly signed, by whoever `with` is).
    pub(crate) fn serve_keys_of(&mut self, of: Addr, with: Addr) {
        let keys = self.server.keys[&with].clone();
        self.server.keys.insert(of, keys);
    }

    /// The bodies of every `Send` client `i` transmitted to `to`.
    pub(crate) fn sends_to(&self, i: usize, to: Addr) -> Vec<Bytes> {
        self.transmitted
            .iter()
            .filter(|(c, _)| *c == i)
            .filter_map(|(_, raw)| match Frame::<ClientMsg>::decode(raw.clone()) {
                Ok(Frame {
                    msg:
                        ClientMsg::Send {
                            to: p,
                            device,
                            body,
                            ..
                        },
                    ..
                }) if Addr::new(p, device) == to => Some(body),
                _ => None,
            })
            .collect()
    }

    /// Makes the server answer `share` with `impostor`'s identity.
    pub(crate) fn hijack_link(&mut self, share: &str, impostor: usize) {
        let link = share.split_once('-').map_or(share, |(l, _)| l).to_owned();
        let peer = self.peer(impostor);
        self.server.links.insert(link, peer);
    }

    /// `joiner` joins a fresh link created by `host`.
    pub(crate) fn pair(&mut self, host: usize, joiner: usize) {
        let link = self.create_link(host);
        self.command(joiner, Command::JoinLink { link });
    }

    pub(crate) fn send_text(&mut self, from: usize, to: usize, text: &str) -> MsgId {
        let msg_id = self.msg_id();
        let peer = self.peer(to);
        self.command(
            from,
            Command::SendText {
                peer,
                msg_id,
                text: text.into(),
                reply_to: None,
            },
        );
        msg_id
    }

    /// Client `i`'s inbox in `epoch`.
    pub(crate) fn inbox_of(&self, i: usize, epoch: u32) -> [u8; 32] {
        let c = &self.clients[i];
        IdentitySeed(c.seed).derive_inbox_key(c.device, epoch).id()
    }

    /// The items waiting in `inbox`.
    pub(crate) fn inbox_items(&self, inbox: &[u8; 32]) -> usize {
        self.server.inboxes.get(inbox).map_or(0, Vec::len)
    }

    /// The items waiting in `inbox`, as the server keeps them.
    pub(crate) fn inbox_contents(&self, inbox: &[u8; 32]) -> &[Bytes] {
        self.server.inboxes.get(inbox).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn texts(&self, i: usize) -> Vec<String> {
        self.clients[i]
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Message(m) if !m.outgoing => match &m.content {
                    cypher_core::Content::Text { text, .. } => Some(text.clone()),
                    cypher_core::Content::File { .. } => None,
                },
                _ => None,
            })
            .collect()
    }

    pub(crate) fn status_of(&self, i: usize, id: MsgId) -> Option<cypher_core::MessageStatus> {
        self.clients[i].events.iter().rev().find_map(|e| match e {
            Event::MessageStatus { msg_id, status } if *msg_id == id => Some(*status),
            _ => None,
        })
    }

    pub(crate) fn has_event(&self, i: usize, pred: impl Fn(&Event) -> bool) -> bool {
        self.clients[i].events.iter().any(pred)
    }

    pub(crate) fn run(&mut self) {
        for _ in 0..100_000 {
            let mut progressed = false;
            for i in 0..self.clients.len() {
                while let Some(input) = self.clients[i].inputs.pop_front() {
                    progressed = true;
                    let c = &mut self.clients[i];
                    let at = self.now.saturating_add(c.ahead);
                    let effects = match (c.core.as_mut(), c.provision.as_mut()) {
                        (Some(core), _) => core.handle(input, at),
                        (None, Some(provision)) => provision.handle(input, self.now),
                        (None, None) => Vec::new(),
                    };
                    self.record(i, at, &effects);
                    self.apply(i, effects);
                }
            }
            while let Some((i, frame)) = self.wire.pop_front() {
                progressed = true;
                self.serve(i, frame);
            }
            while let Some((i, blob)) = self.onion_wire.pop_front() {
                progressed = true;
                self.relay(i, &blob);
            }
            if !progressed {
                return;
            }
        }
        panic!("world did not quiesce");
    }

    /// The relay: hands out the server's key set, or forwards a request
    /// sealed to its onion key.
    fn relay(&mut self, i: usize, frame: &[u8]) {
        let (corr, rest) = frame.split_at(relay::CORR_LEN);
        let (tag, payload) = rest.split_first().expect("relay frames carry a tag");
        match *tag {
            relay::TAG_KEYS => {
                let set = self.key_set();
                let reply = Bytes::from([corr, &set].concat());
                self.clients[i]
                    .inputs
                    .push_back(Input::AnonymousFrame(reply));
            }
            relay::TAG_ONION => {
                let (epoch, sealed) = onion::split_epoch(payload).unwrap();
                assert_eq!(epoch, ONION_EPOCH, "sealed to the published key");
                let opened = onion::open_request(&self.onion_secret, sealed)
                    .expect("relay forwards sealed requests");
                let corr = u64::from_le_bytes(corr.try_into().unwrap());
                self.onion_ctx = Some((i, corr, opened.reply));
                self.serve(i, Bytes::from(opened.frame));
                self.onion_ctx = None;
            }
            other => panic!("unknown relay tag {other}"),
        }
    }

    /// The server's onion key set, signed with its long-term key.
    fn key_set(&self) -> Vec<u8> {
        let key = EpochKey {
            epoch: ONION_EPOCH,
            public: PublicKey::from(&self.onion_secret).to_bytes(),
            not_before: 0,
            not_after: u64::MAX,
        };
        KeySet::sign(&self.server_key, 0, vec![key])
            .unwrap()
            .encode()
    }

    fn apply(&mut self, i: usize, effects: Vec<Effect>) {
        for effect in effects {
            trace_effect(self.trace.as_mut(), i, &effect);
            let c = &mut self.clients[i];
            match effect {
                Effect::Transmit(b) => self.transmit(i, b),
                Effect::Anonymous(b) => self.onion_wire.push_back((i, b)),
                Effect::Persist(StoreOp::Put { table, key, value }) => {
                    c.kv.insert((table as u8, key), value);
                }
                Effect::Persist(StoreOp::Delete { table, key }) => {
                    c.kv.remove(&(table as u8, key));
                }
                Effect::Emit(e) => c.events.push(e),
                Effect::ReadChunk {
                    file_id,
                    index,
                    offset,
                    len,
                    headroom,
                } => match c.sources.get(&file_id) {
                    Some(src) => {
                        let mut buf = vec![0u8; headroom];
                        let start = offset as usize;
                        buf.extend_from_slice(&src[start..start + len as usize]);
                        c.inputs.push_back(Input::ChunkRead {
                            file_id,
                            index,
                            buf,
                        });
                    }
                    None => c.inputs.push_back(Input::ChunkUnavailable { file_id }),
                },
                Effect::OpenSink { file_id, len, .. } => {
                    c.sinks
                        .entry(file_id)
                        .or_insert_with(|| vec![0; len as usize]);
                }
                Effect::WriteChunk {
                    file_id,
                    offset,
                    data,
                } => {
                    let sink = c.sinks.get_mut(&file_id).expect("sink opened before write");
                    let start = offset as usize;
                    sink[start..start + data.len()].copy_from_slice(&data);
                }
                Effect::CloseSink { file_id, complete } => {
                    c.closed.insert(file_id, complete);
                }
                effect @ (Effect::BuildHistory { .. } | Effect::ImportHistory { .. }) => {
                    history_effect(c, effect);
                }
                Effect::Disconnect { .. } => c.connected = false,
            }
        }
    }

    /// A frame client `i` sends, kept for the tests; what was durable at a
    /// `Send` meant to "crash" the client is kept too.
    fn transmit(&mut self, i: usize, b: Bytes) {
        let c = &self.clients[i];
        self.transmitted.push((i, b.clone()));
        if self.crash_rows.is_none()
            && let Some((who, addr)) = self.crash_after_send
            && who == i
            && let Ok(Frame {
                msg: ClientMsg::Send { to, device, .. },
                ..
            }) = Frame::<ClientMsg>::decode(b.clone())
            && Addr::new(to, device) == addr
        {
            self.crash_rows = Some(c.kv.clone());
        }
        if c.connected {
            self.wire.push_back((i, b));
        }
    }

    fn respond(&mut self, to: usize, frame: &Frame<ServerMsg>) {
        match &self.onion_ctx {
            Some((client, corr, reply)) => {
                let mut out = corr.to_le_bytes().to_vec();
                let rng = self.server.rng.as_mut().unwrap();
                out.extend_from_slice(&onion::seal_response(reply, &frame.encode(), rng));
                self.clients[*client]
                    .inputs
                    .push_back(Input::AnonymousFrame(Bytes::from(out)));
            }
            None => self.deliver(to, frame),
        }
    }

    fn deliver(&mut self, to: usize, frame: &Frame<ServerMsg>) {
        if let ServerMsg::Recv { .. } | ServerMsg::RecvSealed { .. } = frame.msg {
            self.server.delivered.push((to, frame.encode()));
        }
        self.clients[to]
            .inputs
            .push_back(Input::Frame(frame.encode()));
    }

    pub(crate) fn inject(&mut self, to: usize, raw: Bytes) {
        self.clients[to].inputs.push_back(Input::Frame(raw));
        self.run();
    }

    fn serve(&mut self, from: usize, raw: Bytes) {
        let Ok(frame) = Frame::<ClientMsg>::decode(raw) else {
            panic!("client sent malformed frame");
        };
        if !self.screen(from, frame.req_id, &frame.msg) {
            self.answer(from, frame);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "mock server: one flat arm per message kind"
    )]
    fn answer(&mut self, from: usize, Frame { req_id, msg }: Frame<ClientMsg>) {
        let reply = |msg| Frame::new(req_id, msg);
        let authed = self
            .server
            .online
            .iter()
            .find(|(_, idx)| **idx == from)
            .map(|(p, _)| *p);
        match msg {
            ClientMsg::Hello { peer, device, .. } => {
                let nonce: [u8; 32] = self.server.rng.as_mut().unwrap().r#gen();
                let addr = Addr::new(peer, device);
                self.server.challenges.insert(from, (nonce, addr));
                self.respond(from, &reply(ServerMsg::Challenge { nonce }));
            }
            ClientMsg::Auth { signature } => {
                let (nonce, addr) = self.server.challenges.remove(&from).expect("hello first");
                let mut signed = SESSION_AUTH_CONTEXT.to_vec();
                signed.extend_from_slice(&nonce);
                signed.extend_from_slice(&addr.device.0.to_le_bytes());
                verify_signature(&addr.peer, &signed, &signature).expect("valid auth signature");
                self.server.online.insert(addr, from);
                self.server.access.remove(&addr);
                self.respond(from, &reply(ServerMsg::Ready));
            }
            ClientMsg::InboxPut { .. }
            | ClientMsg::InboxFetch { .. }
            | ClientMsg::InboxAck { .. }
                if self.onion_ctx.is_some() || authed.is_some() =>
            {
                if self.onion_ctx.is_some() {
                    self.server.onion_inbox_ops += 1;
                } else {
                    self.server.session_inbox_ops += 1;
                }
                self.serve_inbox(from, req_id, msg);
            }
            ClientMsg::FetchKeys { .. }
            | ClientMsg::FetchDevices { .. }
            | ClientMsg::ResolveLink { .. }
                if self.onion_ctx.is_some() || authed.is_some() =>
            {
                if self.onion_ctx.is_some() {
                    self.server.onion_intro_ops += 1;
                } else {
                    self.server.session_intro_ops += 1;
                }
                let answer = self.introduce(msg);
                self.respond(from, &reply(answer));
            }
            ClientMsg::PushKey
            | ClientMsg::PushRegister { .. }
            | ClientMsg::PushUnregister { .. } => {
                if self.onion_ctx.is_none() {
                    self.server.session_push_ops += 1;
                }
                let answer = match msg {
                    ClientMsg::PushRegister {
                        proof,
                        endpoint,
                        p256dh,
                        auth,
                    } => {
                        let extra = push_extra(&endpoint, &p256dh, &auth);
                        self.check_proof(&proof, InboxOp::PushRegister, &extra);
                        self.server.pushes.insert(proof.inbox, endpoint);
                        ServerMsg::Done
                    }
                    ClientMsg::PushUnregister { proof } => {
                        self.check_proof(&proof, InboxOp::PushUnregister, &[]);
                        self.server.pushes.remove(&proof.inbox);
                        ServerMsg::Done
                    }
                    _ => ServerMsg::PushKey { key: [4; 65] },
                };
                self.respond(from, &reply(answer));
            }
            ClientMsg::SendSealed {
                to,
                device,
                token,
                inbox,
                item,
            } if self.onion_ctx.is_some() => {
                let answer = self.send_sealed(Addr::new(to, device), &token, inbox, item);
                self.respond(from, &reply(answer));
            }
            _ if authed.is_none() || self.onion_ctx.is_some() => panic!("unauthenticated request"),
            ClientMsg::SetAccess { hashes } => {
                self.server.access.insert(authed.unwrap(), hashes);
                self.respond(from, &reply(ServerMsg::Done));
            }
            ClientMsg::Ping => self.respond(from, &reply(ServerMsg::Pong)),
            ClientMsg::Send {
                to,
                device,
                want_ack,
                body,
            } => {
                let sender = authed.unwrap();
                let is_chunk = body.first() == Some(&1);
                if body.first() == Some(&0) {
                    self.server.named_messages += 1;
                }
                let lost = !is_chunk
                    && self
                        .server
                        .lose_from
                        .get_mut(&sender.peer)
                        .is_some_and(|left| {
                            let lose = *left > 0;
                            *left = left.saturating_sub(1);
                            lose
                        });
                let dropped = lost
                    || is_chunk
                        && self.server.drop_chunks_every.is_some_and(|n| {
                            self.server.chunk_counter += 1;
                            self.server.chunk_counter.is_multiple_of(n)
                        });
                let status = match self.server.online.get(&Addr::new(to, device)).copied() {
                    Some(idx) if self.clients[idx].connected => {
                        if !dropped {
                            let recv = ServerMsg::Recv {
                                from: sender.peer,
                                device: sender.device,
                                body,
                            };
                            self.deliver(idx, &Frame::new(0, recv));
                        }
                        DeliveryStatus::Delivered
                    }
                    _ => DeliveryStatus::Offline,
                };
                if want_ack {
                    self.respond(from, &reply(ServerMsg::SendAck { status }));
                }
            }
            ClientMsg::PublishKeys {
                base,
                opks,
                replace_opks,
            } => {
                let addr = authed.unwrap();
                let bundle = PrekeyBundle::decode(&[&base[..], &[0]].concat()).unwrap();
                assert_eq!(
                    Addr::new(bundle.identity, bundle.device),
                    addr,
                    "bundle must belong to publisher"
                );
                let entry = self.server.keys.entry(addr).or_default();
                entry.base = base;
                if replace_opks {
                    entry.opks = opks;
                } else {
                    entry.opks.extend(opks);
                }
                let opks_left = entry.opks.len() as u16;
                self.respond(from, &reply(ServerMsg::KeysAck { opks_left }));
            }
            ClientMsg::PublishDevices { list } => {
                let me = authed.unwrap().peer;
                let msg = match DeviceList::decode(&list) {
                    Ok(list) if list.identity() != me => ServerMsg::Error {
                        code: ErrorCode::BadRequest,
                    },
                    Ok(list)
                        if self
                            .server
                            .devices
                            .get(&me)
                            .is_some_and(|stored| stored.version() >= list.version()) =>
                    {
                        ServerMsg::Error {
                            code: ErrorCode::Conflict,
                        }
                    }
                    Ok(list) => {
                        self.server.devices.insert(me, list);
                        ServerMsg::Done
                    }
                    Err(_) => ServerMsg::Error {
                        code: ErrorCode::BadRequest,
                    },
                };
                self.respond(from, &reply(msg));
            }
            ClientMsg::CreateLink => {
                let link = LinkId::random(self.server.rng.as_mut().unwrap());
                self.server
                    .links
                    .insert(link.to_string(), authed.unwrap().peer);
                self.respond(from, &reply(ServerMsg::LinkCreated { link }));
            }
            ClientMsg::Bootstrap => self.respond(
                from,
                &reply(ServerMsg::BootstrapInfo {
                    relay_addr: "relay.test:9443".into(),
                    signing_key: *self.server_key.peer_id().as_bytes(),
                    capabilities: 1,
                }),
            ),
            ClientMsg::InboxPut { .. }
            | ClientMsg::InboxFetch { .. }
            | ClientMsg::InboxAck { .. }
            | ClientMsg::FetchKeys { .. }
            | ClientMsg::FetchDevices { .. }
            | ClientMsg::ResolveLink { .. } => {
                panic!("unauthenticated request")
            }
            ClientMsg::SendSealed { .. } => panic!("a sealed send over the session"),
        }
    }

    /// Logs request `req_id` from client `from`, which must not reuse an
    /// id, and fails it if a test said so; whether it was failed.
    fn screen(&mut self, from: usize, req_id: u32, msg: &ClientMsg) -> bool {
        assert!(
            req_id == 0 || self.clients[from].req_ids.insert(req_id),
            "client {from} used request id {req_id} twice"
        );
        self.server.served.push((from, req_id, msg.clone()));
        let Some(rule) = self
            .server
            .faults
            .iter_mut()
            .find(|r| r.times > 0 && (r.matches)(msg))
        else {
            return false;
        };
        rule.times -= 1;
        if let Fault::Error(code) = rule.fault {
            self.respond(from, &Frame::new(req_id, ServerMsg::Error { code }));
        }
        true
    }

    /// An invite looked up, or a device list or keys fetched: answered alike
    /// whoever asks.
    fn introduce(&mut self, msg: ClientMsg) -> ServerMsg {
        let not_found = ServerMsg::Error {
            code: ErrorCode::NotFound,
        };
        match msg {
            ClientMsg::FetchKeys { peer, device } => {
                self.server.key_fetches += 1;
                match self.server.keys.get_mut(&Addr::new(peer, device)) {
                    Some(k) => ServerMsg::Keys {
                        base: k.base.clone(),
                        opk: k.opks.pop(),
                    },
                    None => not_found,
                }
            }
            ClientMsg::FetchDevices { peer } => {
                self.server.device_fetches += 1;
                match self.server.devices.get(&peer) {
                    Some(list) => ServerMsg::Devices {
                        list: Bytes::from(list.encode()),
                    },
                    None => not_found,
                }
            }
            ClientMsg::ResolveLink { link } => match self.server.links.get(link.as_str()) {
                Some(peer) => ServerMsg::LinkResolved { peer: *peer },
                None => not_found,
            },
            _ => unreachable!("only introductions come here"),
        }
    }

    /// As the server does: the inbox's owner signed `op`, at about now.
    fn check_proof(&self, proof: &InboxProof, op: InboxOp, extra: &[u8]) {
        verify_op(&proof.inbox, op, proof.timestamp, extra, &proof.signature)
            .expect("signed by the inbox's owner");
        assert!(
            (self.now / 1000).abs_diff(proof.timestamp) <= 120,
            "signed just now"
        );
    }

    /// As signaling and the recipient's gateway do together: hands `item`
    /// to the device if it is online and told the token, else leaves it in
    /// `inbox`.
    fn send_sealed(
        &mut self,
        to: Addr,
        token: &[u8; 32],
        inbox: Option<[u8; 32]>,
        item: Bytes,
    ) -> ServerMsg {
        self.server.sealed_sends += 1;
        if self.server.sealed_broken {
            return ServerMsg::Error {
                code: ErrorCode::Unavailable,
            };
        }
        let ack = |status| ServerMsg::SendAck { status };
        let online = self.server.online.get(&to).copied();
        if let Some(idx) = online.filter(|&idx| self.clients[idx].connected) {
            let hash = cypher_wire::access_hash(token);
            if !self
                .server
                .access
                .get(&to)
                .is_some_and(|h| h.contains(&hash))
            {
                return ServerMsg::Error {
                    code: ErrorCode::Unauthorized,
                };
            }
            // The test knows who sent it, though the server would not.
            let sender = self.onion_ctx.as_ref().map(|(i, ..)| self.peer(*i));
            let lost = sender.is_some_and(|sender| {
                self.server.lose_from.get_mut(&sender).is_some_and(|left| {
                    let lose = *left > 0;
                    *left = left.saturating_sub(1);
                    lose
                })
            });
            if !lost {
                self.server.sealed_delivered += 1;
                self.deliver(idx, &Frame::new(0, ServerMsg::RecvSealed { item }));
            }
            return ack(DeliveryStatus::Delivered);
        }
        match inbox {
            Some(inbox) if self.server.open_inboxes.contains(&inbox) => {
                self.server.inboxes.entry(inbox).or_default().push(item);
                if self.server.pushes.contains_key(&inbox) {
                    self.server.signals.push(inbox);
                }
                ack(DeliveryStatus::Queued)
            }
            Some(_) => ServerMsg::Error {
                code: ErrorCode::NotFound,
            },
            None => ack(DeliveryStatus::Offline),
        }
    }

    fn serve_inbox(&mut self, from: usize, req_id: u32, msg: ClientMsg) {
        let reply = |msg| Frame::new(req_id, msg);
        match msg {
            ClientMsg::InboxPut { inbox, item } => {
                if !self.server.open_inboxes.contains(&inbox) {
                    let gone = ServerMsg::Error {
                        code: ErrorCode::NotFound,
                    };
                    self.respond(from, &reply(gone));
                    return;
                }
                self.server.inboxes.entry(inbox).or_default().push(item);
                if self.server.pushes.contains_key(&inbox) {
                    self.server.signals.push(inbox);
                }
                self.respond(from, &reply(ServerMsg::Done));
            }
            ClientMsg::InboxFetch { proof, close } => {
                let op = if close {
                    InboxOp::Close
                } else {
                    InboxOp::Fetch
                };
                self.check_proof(&proof, op, &[]);
                let id = proof.inbox;
                if close {
                    self.server.open_inboxes.remove(&id);
                    self.server.closed_inboxes.insert(id);
                    self.server.pushes.remove(&id);
                } else {
                    self.server.open_inboxes.insert(id);
                    self.server.closed_inboxes.remove(&id);
                }
                let items: Vec<Bytes> = self
                    .server
                    .inboxes
                    .get(&id)
                    .map(|v| {
                        v.iter()
                            .take(cypher_wire::MAX_INBOX_BATCH)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let claim: [u8; 16] = self.server.rng.as_mut().unwrap().r#gen();
                self.server.claims.insert(claim, (id, items.len()));
                self.respond(from, &reply(ServerMsg::InboxBatch { claim, items }));
            }
            ClientMsg::InboxAck { proof, claim } => {
                self.check_proof(&proof, InboxOp::Ack, &claim);
                if let Some((id, n)) = self.server.claims.remove(&claim) {
                    assert_eq!(id, proof.inbox, "acked by the inbox it was claimed from");
                    if let Some(v) = self.server.inboxes.get_mut(&id) {
                        v.drain(..n.min(v.len()));
                    }
                }
                self.respond(from, &reply(ServerMsg::Done));
            }
            _ => unreachable!(),
        }
    }
}

/// What the log keeps of `effect`.
fn seen(effect: &Effect) -> Seen {
    match effect {
        Effect::Persist(StoreOp::Put { table, key, .. } | StoreOp::Delete { table, key }) => {
            Seen::Persist {
                table: *table as u8,
                key: key.clone(),
            }
        }
        Effect::Transmit(b) => Seen::Transmit(b.clone()),
        Effect::Anonymous(b) => Seen::Anonymous(b.clone()),
        Effect::Emit(e) => Seen::Emit(e.clone()),
        Effect::ReadChunk { file_id, .. } => Seen::ReadChunk(*file_id),
        Effect::OpenSink { .. }
        | Effect::WriteChunk { .. }
        | Effect::CloseSink { .. }
        | Effect::BuildHistory { .. }
        | Effect::ImportHistory { .. }
        | Effect::Disconnect { .. } => Seen::Other,
    }
}

/// Adds an effect of client `i` to `trace`, if one is kept: bytes as they
/// are, the rest as it prints.
fn trace_effect(trace: Option<&mut DefaultHasher>, i: usize, effect: &Effect) {
    let Some(trace) = trace else {
        return;
    };
    i.hash(trace);
    match effect {
        Effect::Transmit(b) | Effect::Anonymous(b) => b.hash(trace),
        Effect::Persist(StoreOp::Put { table, key, value }) => (table, key, value).hash(trace),
        Effect::WriteChunk {
            file_id,
            offset,
            data,
        } => (file_id, offset, data).hash(trace),
        other => format!("{other:?}").hash(trace),
    }
    std::mem::discriminant(effect).hash(trace);
}

/// Builds or takes in a history as a driver does, unless the driver is
/// meant to stop first.
fn history_effect(c: &mut Client, effect: Effect) {
    if c.history_held {
        return;
    }
    match effect {
        Effect::BuildHistory { file_id, partial } => {
            let size = (!c.history_fails).then(|| {
                let archive = build_history(c, &partial);
                let size = archive.len() as u64;
                c.sources.insert(file_id, archive);
                size
            });
            c.inputs.push_back(Input::HistoryBuilt { file_id, size });
        }
        Effect::ImportHistory { file_id } => {
            let archive = c.sinks.remove(&file_id).unwrap_or_default();
            let ok = import_history(c, &archive);
            c.inputs.push_back(Input::HistoryImported { file_id, ok });
        }
        _ => unreachable!("only history effects come here"),
    }
}

/// A media row read from an archive, kept until its note is whole.
struct PendingMedia {
    key: Vec<u8>,
    value: Vec<u8>,
    file_id: FileId,
    blob: Vec<u8>,
}

/// The rows a history archive carries, and the voice and video notes of
/// the media rows, as a driver writes them.
fn build_history(c: &Client, partial: &[FileId]) -> Vec<u8> {
    let vault = Vault::new(IdentitySeed(c.seed).derive_storage_key());
    let mut out = Vec::new();
    let mut w = history::Writer::default();
    history::Writer::header(&mut out);
    let rows = |table: Table| {
        c.kv.iter()
            .filter(move |((t, _), _)| *t == table as u8)
            .map(|((_, k), v)| (k, v))
    };
    for (key, value) in rows(Table::Media) {
        let Ok(id) = <[u8; 16]>::try_from(key.as_slice()) else {
            continue;
        };
        let file_id = FileId(id);
        let whole = vault
            .open_media(&file_id, value)
            .ok()
            .zip(c.sinks.get(&file_id))
            .filter(|(m, blob)| m.sealed_len() == blob.len() as u64);
        let Some((_, blob)) = whole.filter(|_| !partial.contains(&file_id)) else {
            continue;
        };
        w.row(&mut out, Table::Media, key, value).unwrap();
        w.blob(&mut out, &file_id, blob.len() as u64);
        out.extend_from_slice(blob);
    }
    for table in [Table::Messages, Table::MessageStatus] {
        for (key, value) in rows(table) {
            w.row(&mut out, table, key, value).unwrap();
        }
    }
    w.end(&mut out);
    out
}

/// Takes in the rows of an archive the store does not have yet, each media
/// row only once its note is written.
fn import_history(c: &mut Client, archive: &[u8]) -> bool {
    let vault = Vault::new(IdentitySeed(c.seed).derive_storage_key());
    let mut reader = history::Reader::new();
    reader.push(archive);
    let mut media: Option<PendingMedia> = None;
    loop {
        match reader.next_item() {
            Ok(Some(history::Item::Row { table, key, value })) => {
                if !history::check_row(&vault, table, key, value) {
                    continue;
                }
                if table == Table::Media {
                    media = Some(PendingMedia {
                        key: key.to_vec(),
                        value: value.to_vec(),
                        file_id: FileId(key.try_into().unwrap()),
                        blob: Vec::new(),
                    });
                } else {
                    c.kv.entry((table as u8, key.to_vec()))
                        .or_insert_with(|| value.to_vec());
                }
            }
            Ok(Some(history::Item::BlobStart { .. } | history::Item::End)) => {}
            Ok(Some(history::Item::BlobData(data))) => {
                if let Some(m) = media.as_mut() {
                    m.blob.extend_from_slice(data);
                }
            }
            Ok(None) => return reader.finish().is_ok(),
            Err(_) => return false,
        }
        let done = media.as_ref().is_some_and(|m| {
            vault
                .open_media(&m.file_id, &m.value)
                .is_ok_and(|k| k.sealed_len() == m.blob.len() as u64)
        });
        if done && let Some(m) = media.take() {
            c.sinks.entry(m.file_id).or_insert(m.blob);
            c.kv.entry((Table::Media as u8, m.key)).or_insert(m.value);
        }
    }
}
