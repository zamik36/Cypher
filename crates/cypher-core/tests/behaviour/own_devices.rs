//! What a device just linked to its identity learns of the messages its
//! siblings send while it is still setting up: before it is online, as it
//! connects, and before their contacts reached it. Kept until they do, also
//! across a restart; not for ever, not when the contact was forgotten
//! meanwhile, and not from strangers.

use bytes::Bytes;
use cypher_core::envelope::{Body, ContactState, Envelope, StatePart, SyncBody};
use cypher_core::link::Provision;
use cypher_core::relay;
use cypher_core::{
    Command, CommandId, Content, MediaKind, Record, StoreOp, StoredMessage, Table, Vault,
    peer_messages,
};
use cypher_crypto::{IdentitySeed, handshake};
use cypher_types::{Addr, DeviceId, MsgId, PeerId};
use cypher_wire::{ClientMsg, Frame, ServerMsg};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

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
    has_sent(w, sibling, laptop, sent);
    assert_eq!(
        messages(w, laptop, w.peer(B)).len(),
        2,
        "nothing else, nothing twice"
    );
}

/// The laptop has the messages, stored as the sibling that knew B all along
/// stored them.
fn has_sent(w: &World, sibling: usize, laptop: usize, sent: [MsgId; 2]) {
    let b = w.peer(B);
    let on_laptop = messages(w, laptop, b);
    let on_sibling = messages(w, sibling, b);
    for id in sent {
        let there = on_sibling.iter().find(|m| m.msg_id == id);
        let here = on_laptop.iter().find(|m| m.msg_id == id);
        assert!(there.is_some(), "the sibling has {id:?}");
        assert_eq!(here, there, "the laptop has {id:?} as the sibling does");
    }
}

/// A sends before the laptop was ever online: the laptop has not published
/// its keys yet, so A cannot reach it until it does. They go out as soon
/// as the laptop first writes to A (asking what it knows), not at A's next
/// look for its keys ten minutes on.
#[test]
fn messages_sent_before_the_new_device_connects_reach_it() {
    let (mut w, sibling, laptop) = linked_offline(2);
    let sent = queue_text_and_file(&mut w);
    w.run();
    w.connect(laptop);
    w.run();
    w.advance(5_000);
    has_both(&w, sibling, laptop, sent);
}

/// The order a slow machine showed: A sends while the laptop is not online
/// yet; the laptop comes up, and B, who heard of it from A, greets it
/// before the laptop has looked at its own list (that look is lost here,
/// so it comes an hour later). Knowing B, as a stranger, does not keep the
/// laptop from asking its siblings: B ends up a contact there, and the
/// laptop has what A sent.
#[test]
fn a_contact_greeting_the_new_device_first_does_not_keep_it_from_asking() {
    let (mut w, sibling, laptop) = linked_offline(2);
    let sent = queue_text_and_file(&mut w);
    w.run();
    let a = w.peer(A);
    w.server.faults.push(Rule::times(
        1,
        move |m| matches!(m, ClientMsg::FetchDevices { peer } if *peer == a),
        Fault::Ignore,
    ));
    w.connect(laptop);
    w.run();
    let b = w.peer(B);
    let hello = Body::Hello {
        inbox: [0; 32],
        name: Some("bob".into()),
        via: None,
        ad: None,
    };
    write_to_laptop(&mut w, B, DeviceId::FIRST, laptop, vec![hello]);
    assert!(w.contact(laptop, b).request, "B greeted the laptop first");

    w.advance(61 * 60_000);
    assert!(!w.contact(laptop, b).request, "the siblings told it of B");
    has_both(&w, sibling, laptop, sent);
}

/// A new device whose request for what its siblings know gets lost asks
/// again, a minute later.
#[test]
fn a_new_device_asks_its_siblings_again() {
    let (mut w, _, laptop) = linked_offline(2);
    let b = w.peer(B);
    let laptop_device = w.clients[laptop].device;
    w.lose_messages_from(laptop, 2);
    w.connect(laptop);
    w.run();
    assert!(!w.has_session(laptop, b), "both requests were lost");
    w.advance(70_000);
    assert!(!w.contact(laptop, b).request, "asked again, and answered");
    assert_eq!(waiting_for(&w, A, laptop_device), 0);
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

/// On a restart, a lost first look at its own device list does not delay
/// asking the siblings already in the persisted list. A sends as they
/// answer: each side starts a session with the other at the same time.
#[test]
fn messages_sent_as_the_new_device_asks_reach_it() {
    let (mut w, sibling, laptop) = linked_offline(2);
    ask_while_siblings_away(&mut w, sibling, laptop);
    let (a_addr, sibling_addr) = (addr(&w, A), addr(&w, sibling));
    let asked_a = w.sends_to(laptop, a_addr).len();
    let asked_sibling = w.sends_to(laptop, sibling_addr).len();
    let a = w.peer(A);
    w.server.faults.push(Rule::times(
        1,
        move |m| matches!(m, ClientMsg::FetchDevices { peer } if *peer == a),
        Fault::Ignore,
    ));
    w.restart(laptop);
    assert_eq!(w.sends_to(laptop, a_addr).len(), asked_a + 1);
    assert_eq!(w.sends_to(laptop, sibling_addr).len(), asked_sibling + 1);

    w.connect(A);
    w.connect(sibling);
    let sent = queue_text_and_file(&mut w);
    w.run();
    w.advance(5_000);
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

/// A takes the laptop off the list before it was ever online: what waited
/// for it is let go then, and nothing reaches it once it comes up, not even
/// after it learns B.
#[test]
fn a_new_device_taken_off_the_list_gets_nothing() {
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
    assert_eq!(waiting_for(&w, A, device), 128);
    w.command(A, Command::UnlinkDevice { device: device.0 });
    assert_eq!(waiting_for(&w, A, device), 0, "let go with the listing");

    w.connect(laptop);
    w.run();
    w.advance(25 * 60_000);
    assert_eq!(waiting_for(&w, A, device), 0);
    assert!(
        messages(&w, laptop, b).is_empty(),
        "nothing A sent reached the laptop"
    );
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

/// The rows client `i` keeps records waiting for their contact in, one
/// each.
fn held_rows(w: &World, i: usize) -> usize {
    w.clients[i]
        .kv
        .keys()
        .filter(|(t, key)| *t == Table::Meta as u8 && key.starts_with(b"held_sent:"))
        .count()
}

#[test]
fn records_wait_for_their_contact_across_a_restart() {
    let (mut w, sibling, laptop, sent) = held_on_the_laptop(2);
    assert_eq!(held_rows(&w, laptop), 2, "a row each");
    w.restart(laptop);
    contacts_arrive(&mut w);
    assert!(w.has_session(laptop, w.peer(B)));
    has_both(&w, sibling, laptop, sent);
    assert_eq!(held_rows(&w, laptop), 0, "gone once stored");
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

/// Device `device` of client `from`'s identity starts a session with the
/// laptop from its published keys and sends it `bodies`.
fn write_to_laptop(w: &mut World, from: usize, device: DeviceId, laptop: usize, bodies: Vec<Body>) {
    let identity = IdentitySeed(w.clients[from].seed).derive_identity();
    let bundle = w.bundle_of(addr(w, laptop));
    let (mut ratchet, init) = handshake::initiate(&identity, device, &bundle, &mut OsRng).unwrap();
    let from = Addr::new(identity.peer_id(), device);
    for body in bodies {
        let envelope = Envelope {
            msg_id: w.msg_id(),
            sent_at_ms: w.now,
            body,
        };
        let (header, ct) = ratchet.encrypt(&envelope.encode(), b"").unwrap();
        let body = relay::message_body(Some(&init), &header, &ct);
        w.inject(laptop, over_session(from, body));
    }
}

/// A record of a text "sent" to `peer`.
fn sent_to(w: &mut World, peer: PeerId, text: String) -> Body {
    Body::Sync(SyncBody::Sent {
        peer,
        msg_id: w.msg_id(),
        sent_at_ms: w.now,
        content: Content::Text {
            text,
            reply_to: None,
        },
    })
}

/// A stranger, C, tells the laptop of `n` messages "sent" to B, as only
/// the laptop's own devices may.
fn stranger_tells_of_sent(w: &mut World, laptop: usize, n: usize) {
    let b = w.peer(B);
    let bodies = (0..n)
        .map(|n| sent_to(w, b, format!("forged {n}")))
        .collect();
    write_to_laptop(w, C, DeviceId::FIRST, laptop, bodies);
}

/// While the laptop waits for its siblings, a device of its identity it
/// does not know of yet tells it of a message sent to C, whom no other
/// device knows; the siblings then answer, without C. After `days`, C
/// writes to the laptop. The world and the laptop.
fn record_for_c_then_state(days: u64) -> (World, usize) {
    let (mut w, sibling, laptop) = linked_offline(3);
    ask_while_siblings_away(&mut w, sibling, laptop);
    let c = w.peer(C);
    let record = sent_to(&mut w, c, "to C, from a new sibling".into());
    write_to_laptop(&mut w, A, DeviceId(77), laptop, vec![record]);
    w.connect(A);
    w.connect(sibling);
    w.advance(10 * 60_000);
    assert!(
        !w.contact(laptop, w.peer(B)).request,
        "the siblings answered"
    );
    assert!(!w.has_session(laptop, c), "without C");
    if days > 0 {
        for i in [A, B, sibling, laptop] {
            w.disconnect(i);
        }
        w.now += days * DAY_MS;
        for i in [A, B, sibling, laptop] {
            w.connect(i);
        }
        w.advance(1_000);
    }
    let hello = Body::Text {
        text: "hi, it's C".into(),
        reply_to: None,
    };
    write_to_laptop(&mut w, C, DeviceId::FIRST, laptop, vec![hello]);
    assert!(w.contact(laptop, c).request, "C wrote as a stranger");
    (w, laptop)
}

/// A State reply without a record's contact leaves the record waiting: it
/// is shown once the contact turns up.
#[test]
fn a_record_outlives_a_state_reply_without_its_contact() {
    let (w, laptop) = record_for_c_then_state(0);
    let to_c: Vec<_> = messages(&w, laptop, w.peer(C))
        .into_iter()
        .filter(|m| m.outgoing)
        .collect();
    assert_eq!(to_c.len(), 1, "{to_c:?}");
}

/// ... until its day is over.
#[test]
fn a_record_outlives_a_state_reply_without_its_contact_until_it_expires() {
    let (w, laptop) = record_for_c_then_state(1);
    let messages = messages(&w, laptop, w.peer(C));
    assert!(messages.iter().all(|m| !m.outgoing), "{messages:?}");
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

fn synthetic_contact(n: u8) -> ContactState {
    ContactState {
        peer: PeerId([n; 32]),
        identity_dh: [n.wrapping_add(1); 32],
        alias: None,
        name: None,
        request: false,
        blocked: false,
        via: None,
        devices: None,
    }
}

fn state_body(id: u8, index: u32, total: u32, contacts: Vec<ContactState>) -> Body {
    Body::Sync(SyncBody::State {
        profile: None,
        contacts,
        links: Vec::new(),
        part: Some(StatePart {
            id: MsgId([id; 16]),
            index,
            total,
        }),
    })
}

#[derive(Serialize, Deserialize)]
struct DevicesV2 {
    list: Option<Vec<u8>>,
    joined: bool,
    announced: u64,
    awaiting_state: bool,
    names: Vec<(u32, String)>,
}

impl Record for DevicesV2 {
    const VERSION: u8 = 2;
}

#[derive(Serialize, Deserialize)]
struct DevicesV1 {
    list: Option<Vec<u8>>,
    joined: bool,
    announced: u64,
    asked: bool,
    names: Vec<(u32, String)>,
}

impl Record for DevicesV1 {
    const VERSION: u8 = 1;
}

fn store_devices_as_v1(w: &mut World, i: usize, asked: bool) {
    let key = b"devices".to_vec();
    let row = (Table::Meta as u8, key.clone());
    let vault = Vault::new(IdentitySeed(w.clients[i].seed).derive_storage_key());
    let current: DevicesV2 = vault
        .open(Table::Meta, &key, &w.clients[i].kv[&row])
        .unwrap();
    assert!(current.awaiting_state);
    let old = DevicesV1 {
        list: current.list,
        joined: current.joined,
        announced: current.announced,
        asked,
        names: current.names,
    };
    let StoreOp::Put { value, .. } = vault.put(Table::Meta, key, &old, &mut OsRng) else {
        unreachable!("a put")
    };
    w.clients[i].kv.insert(row, value);
}

/// A laptop with a persisted device list whose siblings have not answered.
fn waiting_for_a_state_reply() -> (World, usize) {
    let (mut w, sibling, laptop) = linked_offline(2);
    ask_while_siblings_away(&mut w, sibling, laptop);
    (w, laptop)
}

fn state_requests(w: &World, i: usize) -> Vec<MsgId> {
    rows::outbox(w, i)
        .into_iter()
        .filter_map(|row| {
            let envelope = Envelope::decode(&row.envelope).ok()?;
            matches!(envelope.body, Body::Sync(SyncBody::StateRequest)).then_some(row.msg_id)
        })
        .collect()
}

#[test]
fn migrated_devices_reconcile_once_whichever_asked_flag_was_stored() {
    for asked in [false, true] {
        let (mut w, sibling, laptop) = linked_offline(2);
        ask_while_siblings_away(&mut w, sibling, laptop);
        store_devices_as_v1(&mut w, laptop, asked);
        let before = state_requests(&w, laptop);
        w.restart(laptop);
        let migrated = state_requests(&w, laptop);
        assert_eq!(migrated.len(), before.len() + 1, "asked={asked}");
        assert!(before.iter().all(|id| migrated.contains(id)));
        assert!(!w.has_session(laptop, w.peer(B)), "no response yet");

        w.connect(A);
        w.connect(sibling);
        w.run();
        assert!(!w.contact(laptop, w.peer(B)).request, "asked={asked}");

        let answered = state_requests(&w, laptop);
        w.restart(laptop);
        let restarted = state_requests(&w, laptop);
        assert!(
            restarted.iter().all(|id| answered.contains(id)),
            "the answered v2 record must not ask again"
        );
    }
}

/// A restart asks again exactly while the last reply is still incomplete.
/// Counting only across the restart excludes ordinary outbox retransmits.
fn restart_checks_state_incomplete(w: &mut World, laptop: usize, incomplete: bool) {
    let a = addr(w, A);
    let before = w.sends_to(laptop, a).len();
    w.restart(laptop);
    assert_eq!(
        w.sends_to(laptop, a).len(),
        before + usize::from(incomplete),
        "incomplete={incomplete}"
    );
}

#[test]
fn a_state_reply_completes_only_after_all_contacts_arrive() {
    let (mut w, laptop) = waiting_for_a_state_reply();
    let first = (0..50).map(synthetic_contact).collect();
    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![state_body(1, 0, 2, first)],
    );
    restart_checks_state_incomplete(&mut w, laptop, true);

    let first = (0..50).map(synthetic_contact).collect();
    let second = (50..52).map(synthetic_contact).collect();
    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![state_body(2, 0, 2, first), state_body(2, 1, 2, second)],
    );
    restart_checks_state_incomplete(&mut w, laptop, false);
    for n in 0..52 {
        assert!(
            w.has_session(laptop, PeerId([n; 32])),
            "contact {n} arrived"
        );
    }
}

#[test]
fn out_of_order_and_duplicate_state_parts_do_not_complete_early() {
    let (mut w, laptop) = waiting_for_a_state_reply();
    let part = |index| state_body(3, index, 3, vec![synthetic_contact(index as u8)]);
    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![part(0), part(2), part(2)],
    );
    restart_checks_state_incomplete(&mut w, laptop, true);

    // A non-zero part cannot start a reply after the restart. In particular,
    // these three parts do not make a complete set: part two was not kept.
    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![part(2), part(0), part(1)],
    );
    restart_checks_state_incomplete(&mut w, laptop, true);

    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![part(0), part(1), part(2)],
    );
    restart_checks_state_incomplete(&mut w, laptop, false);
}

#[test]
fn a_late_part_of_an_old_state_reply_cannot_complete_a_new_one() {
    let (mut w, laptop) = waiting_for_a_state_reply();
    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![
            state_body(4, 0, 2, vec![synthetic_contact(0)]),
            state_body(5, 0, 3, vec![synthetic_contact(1)]),
            state_body(5, 1, 3, vec![synthetic_contact(2)]),
            state_body(4, 1, 2, vec![synthetic_contact(3)]),
        ],
    );
    restart_checks_state_incomplete(&mut w, laptop, true);

    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![
            state_body(5, 0, 3, vec![synthetic_contact(1)]),
            state_body(5, 1, 3, vec![synthetic_contact(2)]),
            state_body(5, 2, 3, vec![synthetic_contact(4)]),
        ],
    );
    restart_checks_state_incomplete(&mut w, laptop, false);
}

#[test]
fn a_restart_discards_an_incomplete_state_reply() {
    let (mut w, laptop) = waiting_for_a_state_reply();
    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![state_body(6, 0, 2, vec![synthetic_contact(0)])],
    );
    restart_checks_state_incomplete(&mut w, laptop, true);

    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![state_body(6, 1, 2, vec![synthetic_contact(1)])],
    );
    restart_checks_state_incomplete(&mut w, laptop, true);

    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![
            state_body(6, 0, 2, vec![synthetic_contact(0)]),
            state_body(6, 1, 2, vec![synthetic_contact(1)]),
        ],
    );
    restart_checks_state_incomplete(&mut w, laptop, false);
}

#[test]
fn an_unmarked_state_reply_from_an_old_sibling_still_completes() {
    let (mut w, laptop) = waiting_for_a_state_reply();
    let contact = synthetic_contact(200);
    write_to_laptop(
        &mut w,
        A,
        DeviceId::FIRST,
        laptop,
        vec![Body::Sync(SyncBody::State {
            profile: None,
            contacts: vec![contact.clone()],
            links: Vec::new(),
            part: None,
        })],
    );
    restart_checks_state_incomplete(&mut w, laptop, false);
    assert!(w.has_session(laptop, contact.peer));
}
