//! What a device just linked to its identity learns of the messages its
//! siblings send while it is still setting up: before it is online, as it
//! connects, and before their contacts reached it. Kept until they do, also
//! across a restart; not for ever, not when the contact was forgotten
//! meanwhile, and not from strangers.

use bytes::Bytes;
use cypher_core::envelope::{Body, Envelope, SyncBody};
use cypher_core::link::Provision;
use cypher_core::relay;
use cypher_core::{
    Command, CommandId, Content, MediaKind, StoredMessage, Table, Vault, peer_messages,
};
use cypher_crypto::{IdentitySeed, handshake};
use cypher_types::{Addr, DeviceId, MsgId, PeerId};
use cypher_wire::{ClientMsg, Frame, ServerMsg};
use rand::rngs::OsRng;

use super::{addr, rows};
use crate::harness::{Fault, Rule, World};
use crate::{A, B, C, pattern};

const DAY_MS: u64 = 24 * 60 * 60_000;

/// The messages client `i` stored for `peer`, in the order of their keys.
fn messages(w: &World, i: usize, peer: PeerId) -> Vec<StoredMessage> {
    let c = &w.clients[i];
    let vault = Vault::new(IdentitySeed(c.seed).derive_storage_key());
    let (from, to) = peer_messages(&peer);
    c.kv.range((Table::Messages as u8, from)..=(Table::Messages as u8, to))
        .map(|((_, key), sealed)| vault.open_message(key, sealed).unwrap())
        .collect()
}

/// A world of `n` clients where A and B are contacts and A has a second
/// device, which knows B, and a laptop that A links without the history:
/// the laptop holds the identity and is started, but not online yet. The
/// second device and the laptop.
fn linked_offline(n: usize) -> (World, usize, usize) {
    let mut w = World::new(n);
    w.pair(A, B);
    let sibling = w.add_device(A, 2);
    let laptop = w.provision("Laptop");
    let offer = w.offer(laptop);
    w.command(
        A,
        Command::LinkDevice {
            id: CommandId(0),
            offer,
            history: false,
        },
    );
    w.disconnect(laptop);
    let linked = w.clients[laptop]
        .provision
        .as_mut()
        .and_then(Provision::take_linked)
        .expect("the identity was handed over");
    let c = &mut w.clients[laptop];
    c.seed = *linked.seed.as_bytes();
    c.device = linked.device;
    c.provision = None;
    w.boot(laptop);
    (w, sibling, laptop)
}

/// The laptop comes online and asks its siblings, which are away: what it
/// asked waits in their inboxes.
fn ask_while_siblings_away(w: &mut World, sibling: usize, laptop: usize) {
    w.disconnect(A);
    w.disconnect(sibling);
    w.connect(laptop);
    w.run();
    assert!(!w.has_session(laptop, w.peer(B)), "the laptop knows no one");
}

/// Has A send B a text and a file too large to go inline, without running
/// the world: both go out at the next [`World::run`].
fn queue_text_and_file(w: &mut World) -> [MsgId; 2] {
    let b = w.peer(B);
    let text = w.msg_id();
    w.queue(
        A,
        Command::SendText {
            peer: b,
            msg_id: text,
            text: "while the laptop sets up".into(),
            reply_to: None,
        },
    );
    let (file, file_id) = (w.msg_id(), w.file_id());
    let data = pattern(300 * 1024);
    let size = data.len() as u64;
    w.clients[A].sources.insert(file_id, data);
    w.queue(
        A,
        Command::SendFile {
            peer: b,
            msg_id: file,
            file_id,
            name: "notes.pdf".into(),
            mime: "application/pdf".into(),
            size,
            kind: MediaKind::File,
            inline: None,
        },
    );
    [text, file]
}

/// The laptop ends up with both messages, stored as the sibling that knew B
/// all along stored them, and with nothing else for B.
fn has_both(w: &World, sibling: usize, laptop: usize, sent: [MsgId; 2]) {
    let b = w.peer(B);
    let on_laptop = messages(w, laptop, b);
    let on_sibling = messages(w, sibling, b);
    for id in sent {
        let there = on_sibling.iter().find(|m| m.msg_id == id);
        let here = on_laptop.iter().find(|m| m.msg_id == id);
        assert!(there.is_some(), "the sibling has {id:?}");
        assert_eq!(here, there, "the laptop has {id:?} as the sibling does");
    }
    assert_eq!(on_laptop.len(), 2, "nothing else, nothing twice");
}

/// A sends before the laptop was ever online: the laptop has not published
/// its keys yet, so A cannot reach it until it does.
#[test]
fn messages_sent_before_the_new_device_connects_reach_it() {
    let (mut w, sibling, laptop) = linked_offline(2);
    let sent = queue_text_and_file(&mut w);
    w.run();
    w.connect(laptop);
    w.run();
    w.advance(10 * 60_000);
    has_both(&w, sibling, laptop, sent);
}

/// A sends while the laptop connects for the first time.
#[test]
fn messages_sent_as_the_new_device_connects_reach_it() {
    let (mut w, sibling, laptop) = linked_offline(2);
    w.connect(laptop);
    let sent = queue_text_and_file(&mut w);
    w.run();
    w.advance(10 * 60_000);
    has_both(&w, sibling, laptop, sent);
}

/// The messages client `i` has on their way to `device` of its own.
fn waiting_for(w: &World, i: usize, device: DeviceId) -> usize {
    rows::outbox(w, i)
        .iter()
        .filter(|row| row.targets.iter().any(|t| t.device == device))
        .count()
}

/// A laptop that never comes up after it was linked does not keep its
/// siblings' messages for it for ever: the last 128 wait, for a day.
#[test]
fn messages_for_a_new_device_that_never_comes_up_are_let_go() {
    let (mut w, _, laptop) = linked_offline(2);
    let device = w.clients[laptop].device;
    let b = w.peer(B);
    for n in 0..130 {
        let msg_id = w.msg_id();
        w.queue(
            A,
            Command::SendText {
                peer: b,
                msg_id,
                text: format!("number {n}"),
                reply_to: None,
            },
        );
    }
    w.run();
    assert_eq!(waiting_for(&w, A, device), 128, "the last ones wait");
    w.advance(30 * 60_000);
    assert_eq!(waiting_for(&w, A, device), 128, "and keep waiting");

    w.disconnect(A);
    w.now += DAY_MS;
    w.connect(A);
    w.advance(20 * 60_000);
    assert_eq!(waiting_for(&w, A, device), 0, "a day later, none");
}

/// The laptop asked while its siblings were away; A comes back and sends
/// before it reads what the laptop asked.
#[test]
fn messages_sent_before_the_contacts_arrive_reach_the_new_device() {
    let (mut w, sibling, laptop) = linked_offline(2);
    ask_while_siblings_away(&mut w, sibling, laptop);
    w.connect(A);
    let sent = queue_text_and_file(&mut w);
    w.run();
    w.connect(sibling);
    w.run();
    w.advance(10 * 60_000);
    has_both(&w, sibling, laptop, sent);
}

/// In a world of `n` clients, the siblings come back but do not get to
/// what the laptop asked (their inboxes cannot be read): A sends, and the
/// laptop hears of it before it knows B, who is away. The second device and
/// the laptop, and what A sent.
fn held_on_the_laptop(n: usize) -> (World, usize, usize, [MsgId; 2]) {
    let (mut w, sibling, laptop) = linked_offline(n);
    w.disconnect(B);
    ask_while_siblings_away(&mut w, sibling, laptop);
    w.server.faults.push(Rule::always(
        |m| matches!(m, ClientMsg::InboxFetch { .. }),
        Fault::Ignore,
    ));
    w.connect(A);
    w.connect(sibling);
    w.run();
    let sent = queue_text_and_file(&mut w);
    w.run();
    assert!(!w.has_session(laptop, w.peer(B)), "B is not known yet");
    assert!(messages(&w, laptop, w.peer(B)).is_empty());
    (w, sibling, laptop, sent)
}

/// The siblings read their inboxes again, and answer the laptop; B is back.
fn contacts_arrive(w: &mut World) {
    w.server.faults.clear();
    w.connect(B);
    w.advance(10 * 60_000);
}

#[test]
fn records_wait_for_their_contact_across_a_restart() {
    let (mut w, sibling, laptop, sent) = held_on_the_laptop(2);
    w.restart(laptop);
    contacts_arrive(&mut w);
    assert!(w.has_session(laptop, w.peer(B)));
    has_both(&w, sibling, laptop, sent);
}

#[test]
fn records_whose_contact_does_not_come_within_a_day_are_let_go() {
    let (mut w, sibling, laptop, _) = held_on_the_laptop(2);
    for i in [A, sibling, laptop] {
        w.disconnect(i);
    }
    w.now += DAY_MS;
    for i in [A, sibling, laptop] {
        w.connect(i);
    }
    w.advance(1_000);
    contacts_arrive(&mut w);
    assert!(!w.contact(laptop, w.peer(B)).request, "B came, too late");
    assert!(messages(&w, laptop, w.peer(B)).is_empty());
}

/// A forgets B before the laptop knows B. B comes back and greets the
/// laptop, which takes B for a stranger, without what A sent B before.
#[test]
fn records_for_a_contact_forgotten_meanwhile_are_let_go() {
    let (mut w, _, laptop, _) = held_on_the_laptop(2);
    let b = w.peer(B);
    w.command(A, Command::RemovePeer { peer: b });
    contacts_arrive(&mut w);
    assert!(w.contact(laptop, b).request, "B is a stranger here now");
    let on_laptop = messages(&w, laptop, b);
    assert!(
        on_laptop.iter().all(|m| !m.outgoing),
        "nothing A sent: {on_laptop:?}"
    );
}

/// A frame client `to` gets from `from` over the session.
fn over_session(from: Addr, body: Vec<u8>) -> Bytes {
    Frame::new(
        0,
        ServerMsg::Recv {
            from: from.peer,
            device: from.device,
            body: Bytes::from(body),
        },
    )
    .encode()
}

/// A stranger, C, starts a session with the laptop from its published keys
/// and tells it of `n` messages "sent" to B, as only the laptop's own
/// devices may.
fn stranger_tells_of_sent(w: &mut World, laptop: usize, n: usize) {
    let identity = IdentitySeed(w.clients[C].seed).derive_identity();
    let bundle = w.bundle_of(addr(w, laptop));
    let (mut ratchet, init) =
        handshake::initiate(&identity, DeviceId::FIRST, &bundle, &mut OsRng).unwrap();
    let from = Addr::new(identity.peer_id(), DeviceId::FIRST);
    let b = w.peer(B);
    for n in 0..n {
        let msg_id = w.msg_id();
        let envelope = Envelope {
            msg_id: w.msg_id(),
            sent_at_ms: w.now,
            body: Body::Sync(SyncBody::Sent {
                peer: b,
                msg_id,
                sent_at_ms: w.now,
                content: Content::Text {
                    text: format!("forged {n}"),
                    reply_to: None,
                },
            }),
        };
        let (header, ct) = ratchet.encrypt(&envelope.encode(), b"").unwrap();
        let body = relay::message_body(Some(&init), &header, &ct);
        w.inject(laptop, over_session(from, body));
    }
}

/// Only the laptop's own devices tell it of messages sent: a stranger's
/// records, however many, are not kept, nor push out the laptop's own.
#[test]
fn a_stranger_cannot_fill_the_records_kept() {
    let (mut w, sibling, laptop, sent) = held_on_the_laptop(3);
    stranger_tells_of_sent(&mut w, laptop, 300);
    let c = w.peer(C);
    assert!(w.contact(laptop, c).request, "the stranger got through");
    contacts_arrive(&mut w);
    has_both(&w, sibling, laptop, sent);
}
