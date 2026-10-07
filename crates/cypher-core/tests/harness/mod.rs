//! Deterministic in-memory world: a mock server with the gateway/signaling
//! semantics of wire v2 and client drivers around real `Core` instances.

use std::collections::{BTreeMap, HashMap, VecDeque};

use bytes::Bytes;
use cypher_core::{Command, Core, Effect, Event, Input, Snapshot, StoreOp, Table, Vault};
use cypher_crypto::identity::verify_signature;
use cypher_crypto::onion::{self, ReplyKey};
use cypher_crypto::{IdentitySeed, PrekeyBundle};
use cypher_types::{Addr, DeviceId, FileId, LinkId, MsgId, PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, DeliveryStatus, ErrorCode, Frame, ServerMsg};
use rand::{Rng as _, SeedableRng};
use rand_chacha::ChaCha20Rng;
use x25519_dalek::{PublicKey, StaticSecret};

pub(crate) type Rng = ChaCha20Rng;

#[derive(Default)]
struct Keys {
    base: Bytes,
    opks: Vec<(u32, [u8; 32])>,
}

#[derive(Default)]
pub(crate) struct Server {
    online: HashMap<Addr, usize>,
    challenges: HashMap<usize, ([u8; 32], Addr)>,
    keys: HashMap<Addr, Keys>,
    links: HashMap<String, PeerId>,
    inboxes: HashMap<[u8; 32], Vec<Bytes>>,
    claims: HashMap<[u8; 16], ([u8; 32], usize)>,
    /// Every `Recv` delivered, for replay/tamper attacks.
    pub delivered: Vec<(usize, Bytes)>,
    pub drop_chunks_every: Option<usize>,
    /// Messages (not chunks) still to lose per sender: the server takes and
    /// acknowledges them, the recipient never sees them.
    pub lose_from: HashMap<PeerId, usize>,
    chunk_counter: usize,
    rng: Option<Rng>,
    pub session_inbox_ops: usize,
    pub onion_inbox_ops: usize,
    /// Push requests that came over the session instead of the relay.
    pub session_push_ops: usize,
    /// Push endpoint per inbox, and the inboxes signalled so far.
    pub pushes: HashMap<[u8; 32], String>,
    pub signals: Vec<[u8; 32]>,
}

pub(crate) struct Client {
    pub seed: [u8; 32],
    pub core: Option<Core<Rng>>,
    pub kv: BTreeMap<(u8, Vec<u8>), Vec<u8>>,
    pub sources: HashMap<FileId, Vec<u8>>,
    pub sinks: HashMap<FileId, Vec<u8>>,
    pub closed: HashMap<FileId, bool>,
    pub events: Vec<Event>,
    pub connected: bool,
    inputs: VecDeque<Input>,
}

pub(crate) struct World {
    pub now: u64,
    pub server: Server,
    pub clients: Vec<Client>,
    pub relay_up: bool,
    wire: VecDeque<(usize, Bytes)>,
    onion_wire: VecDeque<(usize, Bytes)>,
    onion_secret: StaticSecret,
    onion_ctx: Option<(usize, u64, ReplyKey)>,
    rng: Rng,
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
            onion_ctx: None,
            rng: Rng::seed_from_u64(1),
        };
        for _ in 0..n {
            let seed: [u8; 32] = rng.r#gen();
            world.clients.push(Client {
                seed,
                core: None,
                kv: BTreeMap::new(),
                sources: HashMap::new(),
                sinks: HashMap::new(),
                closed: HashMap::new(),
                events: Vec::new(),
                connected: false,
                inputs: VecDeque::new(),
            });
        }
        for i in 0..n {
            world.boot(i);
            world.connect(i);
        }
        world.run();
        world
    }

    pub(crate) fn peer(&self, i: usize) -> PeerId {
        self.clients[i].core.as_ref().unwrap().peer_id()
    }

    /// (Re)creates the core from the client's persisted key-value state.
    pub(crate) fn boot(&mut self, i: usize) {
        let c = &mut self.clients[i];
        let pick = |t: Table| -> Vec<(Vec<u8>, Vec<u8>)> {
            c.kv.iter()
                .filter(|((table, _), _)| *table == t as u8)
                .map(|((_, k), v)| (k.clone(), v.clone()))
                .collect()
        };
        let snapshot = Snapshot {
            meta: pick(Table::Meta),
            peers: pick(Table::Peers),
            outbox: pick(Table::Outbox),
            transfers: pick(Table::Transfers),
        };
        let rng = Rng::seed_from_u64(self.rng.r#gen());
        let (core, effects) =
            Core::restore(&IdentitySeed(c.seed), &snapshot, self.now, rng).unwrap();
        c.core = Some(core);
        c.connected = false;
        self.apply(i, effects);
    }

    pub(crate) fn connect(&mut self, i: usize) {
        self.clients[i].connected = true;
        self.clients[i].inputs.push_back(Input::Connected);
        self.clients[i]
            .inputs
            .push_back(Input::AnonymousChannel { up: self.relay_up });
    }

    pub(crate) fn disconnect(&mut self, i: usize) {
        let peer = self.peer(i);
        self.server
            .online
            .retain(|p, idx| !(p.peer == peer && *idx == i));
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
                    let effects = self.clients[i]
                        .core
                        .as_mut()
                        .unwrap()
                        .handle(input, self.now);
                    self.apply(i, effects);
                }
            }
            while let Some((i, frame)) = self.wire.pop_front() {
                progressed = true;
                self.serve(i, frame);
            }
            while let Some((i, blob)) = self.onion_wire.pop_front() {
                progressed = true;
                let (corr, sealed) = blob.split_at(8);
                let opened = onion::open_request(&self.onion_secret, sealed)
                    .expect("relay forwards sealed requests");
                let corr = u64::from_le_bytes(corr.try_into().unwrap());
                self.onion_ctx = Some((i, corr, opened.reply));
                self.serve(i, Bytes::from(opened.frame));
                self.onion_ctx = None;
            }
            if !progressed {
                return;
            }
        }
        panic!("world did not quiesce");
    }

    fn apply(&mut self, i: usize, effects: Vec<Effect>) {
        for effect in effects {
            let c = &mut self.clients[i];
            match effect {
                Effect::Transmit(b) => {
                    if c.connected {
                        self.wire.push_back((i, b));
                    }
                }
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
                Effect::Disconnect { .. } => c.connected = false,
            }
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
        if let ServerMsg::Recv { .. } = frame.msg {
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

    #[expect(
        clippy::too_many_lines,
        reason = "mock server: one flat arm per message kind"
    )]
    fn serve(&mut self, from: usize, raw: Bytes) {
        let Ok(Frame { req_id, msg }) = Frame::<ClientMsg>::decode(raw) else {
            panic!("client sent malformed frame");
        };
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
            ClientMsg::PushKey
            | ClientMsg::PushRegister { .. }
            | ClientMsg::PushUnregister { .. } => {
                if self.onion_ctx.is_none() {
                    self.server.session_push_ops += 1;
                }
                let answer = match msg {
                    ClientMsg::PushRegister {
                        secret, endpoint, ..
                    } => {
                        let inbox = cypher_wire::inbox_id(&secret);
                        self.server.pushes.insert(inbox, endpoint);
                        ServerMsg::Done
                    }
                    ClientMsg::PushUnregister { secret } => {
                        self.server.pushes.remove(&cypher_wire::inbox_id(&secret));
                        ServerMsg::Done
                    }
                    _ => ServerMsg::PushKey { key: [4; 65] },
                };
                self.respond(from, &reply(answer));
            }
            _ if authed.is_none() || self.onion_ctx.is_some() => panic!("unauthenticated request"),
            ClientMsg::Ping => self.respond(from, &reply(ServerMsg::Pong)),
            ClientMsg::Send {
                to,
                device,
                want_ack,
                body,
            } => {
                let sender = authed.unwrap();
                let is_chunk = body.first() == Some(&1);
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
            ClientMsg::FetchKeys { peer, device } => {
                let msg = match self.server.keys.get_mut(&Addr::new(peer, device)) {
                    Some(k) => ServerMsg::Keys {
                        base: k.base.clone(),
                        opk: k.opks.pop(),
                    },
                    None => ServerMsg::Error {
                        code: ErrorCode::NotFound,
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
            ClientMsg::ResolveLink { link } => {
                let msg = match self.server.links.get(link.as_str()) {
                    Some(peer) => ServerMsg::LinkResolved { peer: *peer },
                    None => ServerMsg::Error {
                        code: ErrorCode::NotFound,
                    },
                };
                self.respond(from, &reply(msg));
            }
            ClientMsg::Bootstrap => self.respond(
                from,
                &reply(ServerMsg::BootstrapInfo {
                    relay_addr: "relay.test:9443".into(),
                    onion_key: PublicKey::from(&self.onion_secret).to_bytes(),
                    capabilities: 1,
                }),
            ),
            ClientMsg::InboxPut { .. }
            | ClientMsg::InboxFetch { .. }
            | ClientMsg::InboxAck { .. } => {
                panic!("unauthenticated inbox request")
            }
        }
    }

    fn serve_inbox(&mut self, from: usize, req_id: u32, msg: ClientMsg) {
        let reply = |msg| Frame::new(req_id, msg);
        match msg {
            ClientMsg::InboxPut { inbox, item } => {
                self.server.inboxes.entry(inbox).or_default().push(item);
                if self.server.pushes.contains_key(&inbox) {
                    self.server.signals.push(inbox);
                }
                self.respond(from, &reply(ServerMsg::Done));
            }
            ClientMsg::InboxFetch { secret } => {
                let id = cypher_wire::inbox_id(&secret);
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
            ClientMsg::InboxAck { secret, claim } => {
                if let Some((id, n)) = self.server.claims.remove(&claim) {
                    assert_eq!(id, cypher_wire::inbox_id(&secret));
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
