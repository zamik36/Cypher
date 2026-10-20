//! Messages on their way: the inbox a device away reads, attempts again
//! and how far apart, receipts per device, and what the outbox keeps.

use std::collections::HashSet;

use cypher_core::{Command, Event, MessageStatus};
use cypher_crypto::{DeviceList, IdentitySeed};
use cypher_types::{Addr, DeviceId, MsgId};
use cypher_wire::{ClientMsg, ErrorCode, Frame};
use proptest::prelude::*;

use super::{addr, inbox_items_from, message_to, rows, settled};
use crate::harness::{Fault, Rule, Seen, World};
use crate::{A, B, C, D, paired, weeks_pass};

/// Time between two ticks of the test world.
const TICK_MS: u64 = 500;

/// A contact with a second device that has a session with the first.
fn with_sibling() -> (World, usize) {
    let mut w = paired();
    let a2 = w.add_device(A, 2);
    w.advance(60 * 60_000);
    (w, a2)
}

/// Whether client `i` shows message `id`.
fn shows(w: &World, i: usize, id: MsgId) -> bool {
    w.has_event(i, |e| matches!(e, Event::Message(m) if m.msg_id == id))
}

/// A message that goes with our name (the contact's token was refused) to a
/// device that is away is left in the inbox that device reads in the
/// current epoch, sealed so that only the contact opens it: the second of
/// what it announced a week ago, once that week is over.
#[test]
fn a_named_message_to_a_device_away_is_left_in_its_current_inbox() {
    let mut w = paired();
    let b = addr(&w, B);
    w.server.access.insert(b, Vec::new());
    w.send_text(A, B, "token refused");
    w.disconnect(B);
    weeks_pass(&mut w, 1, &[B]);
    let (this, last) = (w.inbox_of(B, 1), w.inbox_of(B, 0));
    let held = (w.inbox_items(&this), w.inbox_items(&last));
    let (named, onion) = (w.server.named_messages, w.server.onion_inbox_ops);

    let id = w.send_text(A, B, "left in the inbox");
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Queued));
    assert_eq!(w.server.named_messages, named + 1, "tried as ourselves");
    assert!(
        w.server.onion_inbox_ops > onion,
        "then left through the relay"
    );
    assert_eq!(w.inbox_items(&this), held.0 + 1, "in this epoch's inbox");
    assert_eq!(w.inbox_items(&last), held.1, "not in last epoch's");

    w.connect(B);
    w.advance(1_000);
    assert!(w.texts(B).contains(&"left in the inbox".to_owned()));
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
}

/// The copy of a sent message for another device of ours that is away is
/// left in that device's inbox, sealed to our identity, and shown there
/// once it is back.
#[test]
fn a_copy_for_an_own_device_away_is_left_in_its_inbox() {
    let (mut w, a2) = with_sibling();
    w.disconnect(a2);
    let a = addr(&w, A);
    let id = w.send_text(A, B, "sent while the laptop sleeps");
    assert!(inbox_items_from(&w, a2, 0, a) > 0);

    w.connect(a2);
    w.advance(1_000);
    assert!(w.has_event(
        a2,
        |e| matches!(e, Event::Message(m) if m.msg_id == id && m.outgoing)
    ));
}

/// Where the user requires the relay and it is down, nothing is left in an
/// inbox over the session: the copy waits, is tried again now and then,
/// and goes through the relay once it is back.
#[test]
fn without_the_required_relay_an_inbox_copy_waits_for_it() {
    let (mut w, a2) = with_sibling();
    w.command(
        A,
        Command::SetAnonymity {
            require_onion: true,
        },
    );
    w.set_relay(A, false);
    w.disconnect(a2);
    let a = addr(&w, A);
    let session_ops = w.server.session_inbox_ops;
    let id = w.send_text(A, B, "kept back");
    w.advance(5 * 60_000);
    assert_eq!(inbox_items_from(&w, a2, 0, a), 0);
    assert_eq!(
        w.server.session_inbox_ops, session_ops,
        "never over the session"
    );

    w.set_relay(A, true);
    w.advance(10 * 60_000);
    assert!(inbox_items_from(&w, a2, 0, a) > 0, "through the relay");
    w.connect(a2);
    w.advance(1_000);
    assert!(shows(&w, a2, id));
}

/// Just after connecting, while the relay may still come up, a copy for an
/// own device away is not left yet: it is tried again within seconds, and
/// once the wait for the relay is over it goes into the inbox over the
/// session.
#[test]
fn while_the_relay_may_come_up_an_inbox_copy_is_tried_again_soon() {
    let (mut w, a2) = with_sibling();
    w.disconnect(a2);
    w.relay_up = false;
    w.disconnect(A);
    w.connect(A);
    w.run();
    let (a, to_a2) = (addr(&w, A), addr(&w, a2));
    let tries = w.sends_to(A, to_a2).len();
    let id = w.send_text(A, B, "while the relay comes up");
    w.advance(9_000);
    let tried = w.sends_to(A, to_a2).len() - tries;
    assert!(tried >= 3, "tried {tried} times in nine seconds");
    assert_eq!(inbox_items_from(&w, a2, 0, a), 0, "not left while waiting");

    w.advance(30_000);
    assert!(inbox_items_from(&w, a2, 0, a) > 0);
    w.relay_up = true;
    w.connect(a2);
    w.advance(1_000);
    assert!(shows(&w, a2, id));
}

/// A message the server refuses (an error, not "away") goes again a few
/// seconds later, not at once.
#[test]
fn a_refused_message_is_sent_again_seconds_later() {
    let (mut w, a2) = with_sibling();
    let to_a2 = addr(&w, a2);
    w.server.faults.push(Rule::times(
        1,
        move |m| message_to(m, to_a2),
        Fault::Error(ErrorCode::Internal),
    ));
    let id = w.send_text(A, B, "refused once");
    w.advance(2_000);
    assert!(!shows(&w, a2, id), "not at once");
    w.advance(3_500);
    assert!(shows(&w, a2, id));
}

/// A message the server never answers is not sent again while its request
/// waits; once the request times out, it is.
#[test]
fn an_unanswered_message_goes_again_only_after_its_request_times_out() {
    let (mut w, a2) = with_sibling();
    let to_a2 = addr(&w, a2);
    w.server
        .faults
        .push(Rule::times(1, move |m| message_to(m, to_a2), Fault::Ignore));
    let tries = w.sends_to(A, to_a2).len();
    let id = w.send_text(A, B, "lost on the way");
    assert_eq!(w.sends_to(A, to_a2).len(), tries + 1);
    w.advance(15_000 - TICK_MS);
    assert_eq!(
        w.sends_to(A, to_a2).len(),
        tries + 1,
        "not again while waiting for the answer"
    );
    w.advance(6_000);
    assert_eq!(w.sends_to(A, to_a2).len(), tries + 2);
    assert!(shows(&w, a2, id));
}

/// When a time each message to a device reached the server.
fn send_times(w: &World, from: usize, to: Addr) -> Vec<u64> {
    let log = w.log.as_ref().expect("the log is kept");
    log.iter()
        .filter(|batch| batch.client == from)
        .flat_map(|batch| {
            batch.effects.iter().filter_map(move |e| match e {
                Seen::Transmit(raw) => Frame::<ClientMsg>::decode(raw.clone())
                    .ok()
                    .filter(|f| message_to(&f.msg, to))
                    .map(|_| batch.at),
                _ => None,
            })
        })
        .collect()
}

/// Each failed attempt doubles the wait before the next, which falls in
/// the upper half of it: from half a minute for a device away, up to ten
/// minutes; never sooner.
#[test]
fn attempts_back_off_until_ten_minutes_apart() {
    const AWAY_MS: u64 = 30_000;
    const MOST_MS: u64 = 10 * 60_000;
    let (mut w, a2) = with_sibling();
    w.command(
        A,
        Command::SetAnonymity {
            require_onion: true,
        },
    );
    w.set_relay(A, false);
    w.disconnect(a2);
    w.log = Some(Vec::new());
    w.send_text(A, B, "again and again");
    w.advance(60 * 60_000);

    let times = send_times(&w, A, addr(&w, a2));
    let gaps: Vec<u64> = times.windows(2).map(|t| t[1] - t[0]).collect();
    assert!(gaps.len() >= 6, "{gaps:?}");
    for (k, gap) in gaps.iter().enumerate() {
        let backoff = AWAY_MS.saturating_mul(1 << k.min(10)).min(MOST_MS);
        assert!(
            (backoff / 2..=backoff + TICK_MS).contains(gap),
            "attempt {k} after {gap} ms: {gaps:?}"
        );
    }
    assert!(gaps.iter().any(|g| *g >= MOST_MS / 2), "reaches the cap");
}

/// A device's token used up for the hour: messages to it go as ourselves
/// for that hour, then sealed again.
#[test]
fn a_used_up_token_sends_as_oneself_for_an_hour() {
    let mut w = settled();
    w.server.faults.push(Rule::times(
        1,
        |m| matches!(m, ClientMsg::SendSealed { .. }),
        Fault::Error(ErrorCode::RateLimited),
    ));
    let named = w.server.named_messages;
    w.send_text(A, B, "used up");
    assert_eq!(w.server.named_messages, named + 1);

    w.advance(59 * 60_000);
    let named = w.server.named_messages;
    w.send_text(A, B, "within the hour");
    assert_eq!(w.server.named_messages, named + 1, "still as ourselves");

    w.advance(2 * 60_000);
    let named = w.server.named_messages;
    w.send_text(A, B, "the hour is over");
    assert_eq!(w.server.named_messages, named, "sealed again");
    assert_eq!(
        w.texts(B),
        ["used up", "within the hour", "the hour is over"]
    );
}

/// After the server could not reach the device's gateway at all, messages
/// go as their sender for five minutes, not shorter.
#[test]
fn a_broken_sealed_path_is_not_tried_for_five_minutes() {
    let mut w = settled();
    w.server.sealed_broken = true;
    w.send_text(A, B, "the long way is down");
    w.server.sealed_broken = false;
    w.advance(4 * 60_000);
    let named = w.server.named_messages;
    w.send_text(A, B, "still the short way");
    assert_eq!(
        w.server.named_messages,
        named + 2,
        "the text and its receipt"
    );
}

/// Once every device answered, nothing of a message stays: control
/// messages are gone once the server took them, and a message the user
/// sees keeps only the record of its receipts, without its text.
#[test]
fn settled_messages_leave_no_content_in_the_outbox() {
    let mut w = paired();
    let ids = [w.send_text(A, B, "one"), w.send_text(A, B, "two")];
    w.send_text(B, A, "an answer, for a receipt");
    let b = w.peer(B);
    let rows = rows::outbox(&w, A);
    assert_eq!(rows.len(), ids.len(), "only the texts' records");
    for row in rows {
        assert!(ids.contains(&row.msg_id) && row.peer == b && row.tracked);
        assert!(row.targets.is_empty() && row.envelope.is_empty());
    }
}

/// A receipt from another contact for a message does not settle it: the
/// message still waits for its own recipient.
#[test]
fn a_receipt_from_someone_else_leaves_the_message_waiting() {
    let mut w = World::new(3);
    w.pair(A, B);
    w.pair(A, C);
    w.disconnect(B);
    let id = w.send_text(A, B, "for B");
    let a = w.peer(A);
    w.command(
        C,
        Command::MarkRead {
            peer: a,
            ids: vec![id],
        },
    );
    let row = rows::outbox_row(&w, A, id).expect("still on its way");
    assert!(
        row.targets
            .iter()
            .any(|t| t.device == DeviceId::FIRST && t.awaiting)
    );
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Queued));

    w.connect(B);
    w.advance(1_000);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
}

/// The server has no keys for one device of a contact: the message stops
/// going to it, and its keys are not asked for again.
#[test]
fn a_device_without_keys_is_no_longer_a_target() {
    let mut w = World::new(2);
    let b2 = w.add_device(B, 2);
    w.advance(60 * 60_000);
    let b = w.peer(B);
    w.remove_keys(Addr::new(b, DeviceId(2)));
    w.disconnect(b2);
    w.pair(B, A);
    let id = w.send_text(A, B, "to every device of B");
    assert_eq!(w.texts(B), ["to every device of B"]);
    let fetches = w.server.key_fetches;
    w.advance(60 * 60_000);
    assert_eq!(w.server.key_fetches, fetches, "not asked for again");
    let row = rows::outbox_row(&w, A, id).expect("its receipts' record");
    assert!(row.targets.is_empty());
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
}

/// A message the server never took for any device of the contact, all of
/// them taken off the contact's list meanwhile, leaves nothing behind: not
/// even a record of it. (The contact's inbox is "gone" each time, so the
/// sender looks up their devices and finds the new list.)
#[test]
fn a_message_no_device_took_leaves_no_record() {
    let mut w = settled();
    let b = w.peer(B);
    w.server.faults.push(Rule::always(
        move |m| matches!(m, ClientMsg::SendSealed { to, .. } if *to == b),
        Fault::Error(ErrorCode::NotFound),
    ));
    w.server.faults.push(Rule::always(
        move |m| matches!(m, ClientMsg::Send { to, .. } if *to == b),
        Fault::Error(ErrorCode::Internal),
    ));
    let id = w.send_text(A, B, "to nobody");
    assert_eq!(w.status_of(A, id), None, "not taken yet");
    assert!(rows::outbox_row(&w, A, id).is_some());

    let identity = IdentitySeed(w.clients[B].seed).derive_identity();
    let version = w.server.devices[&b].version() + 1;
    let elsewhere = DeviceList::sign(&identity, version, [DeviceId(3)]).unwrap();
    w.server.devices.insert(b, elsewhere);
    w.advance(60 * 60_000);
    assert!(rows::outbox_row(&w, A, id).is_none());
}

/// A device a contact took off their list is no longer written to: what
/// waited for it stops waiting. What waits for a device of the same number
/// of another contact still does.
#[test]
fn a_device_taken_off_a_contacts_list_is_no_longer_waited_for() {
    let mut w = World::new(3);
    let a2 = w.add_device(A, 2);
    let c2 = w.add_device(C, 2);
    w.pair(A, B);
    w.pair(C, B);
    w.advance(60 * 60_000);
    w.disconnect(a2);
    w.disconnect(c2);
    let to_a = w.send_text(B, A, "to A");
    let to_c = w.send_text(B, C, "to C");
    let waits_for_2 = |w: &World, id| {
        rows::outbox_row(w, B, id).is_some_and(|row| {
            row.targets
                .iter()
                .any(|t| t.device == DeviceId(2) && t.awaiting)
        })
    };
    assert!(waits_for_2(&w, to_a) && waits_for_2(&w, to_c));

    let a = w.peer(A);
    let identity = IdentitySeed(w.clients[A].seed).derive_identity();
    let shorter = w.server.devices[&a]
        .without(&identity, DeviceId(2))
        .unwrap();
    w.server.devices.insert(a, shorter);
    w.advance(60 * 60_000);
    assert!(!waits_for_2(&w, to_a), "A's second device is gone");
    assert!(waits_for_2(&w, to_c), "C's second device is still listed");
}

/// The inbox of a contact's device away is gone: the message is tried
/// again later, and the contact's devices are looked up, as the device may
/// have been taken off their list.
#[test]
fn an_inbox_found_gone_has_the_contacts_devices_looked_up() {
    let mut w = settled();
    w.disconnect(B);
    w.advance(60 * 60_000);
    let fetches = w.server.device_fetches;
    w.send_text(A, B, "into the inbox");
    assert_eq!(w.server.device_fetches, fetches, "not for an inbox there");

    w.advance(60 * 60_000);
    w.server.faults.push(Rule::times(
        1,
        |m| matches!(m, ClientMsg::InboxPut { .. } | ClientMsg::SendSealed { .. }),
        Fault::Error(ErrorCode::NotFound),
    ));
    let fetches = w.server.device_fetches;
    let id = w.send_text(A, B, "into an inbox gone");
    assert_eq!(w.server.device_fetches, fetches + 1);
    assert_eq!(w.status_of(A, id), None, "not taken yet");
    w.advance(60_000);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Queued));
}

/// The second device's receipt for a message both got tells the user
/// nothing new, yet the message stops waiting for it.
#[test]
fn a_second_devices_receipt_is_kept_but_not_told_again() {
    let mut w = World::new(2);
    let b2 = w.add_device(B, 2);
    w.pair(B, A);
    w.advance(60 * 60_000);
    w.disconnect(b2);
    let id = w.send_text(A, B, "to both of B's");
    w.connect(b2);
    w.advance(10_000);
    assert!(shows(&w, b2, id));
    let told = w.clients[A]
        .events
        .iter()
        .filter(|e| {
            matches!(e, Event::MessageStatus { msg_id, status: MessageStatus::Delivered }
                if *msg_id == id)
        })
        .count();
    assert_eq!(told, 1, "delivered is told once");
    let row = rows::outbox_row(&w, A, id).expect("its receipts' record");
    assert!(row.targets.is_empty(), "both devices answered");
}

/// Blocking a contact, or forgetting one, drops what waits for them:
/// nothing written to them before goes out, also after unblocking. What
/// waits for others still goes.
#[test]
fn blocking_or_forgetting_a_contact_drops_what_waits_for_them() {
    let mut w = World::new(4);
    w.pair(A, B);
    w.pair(A, C);
    w.pair(A, D);
    let (b, c, d) = (w.peer(B), w.peer(C), w.peer(D));
    w.disconnect(A);
    w.send_text(A, B, "to B, then blocked");
    w.send_text(A, C, "to C, then forgotten");
    w.send_text(A, D, "to D");
    w.command(A, Command::BlockPeer { peer: b });
    w.command(A, Command::UnblockPeer { peer: b });
    w.command(A, Command::RemovePeer { peer: c });
    w.connect(A);
    w.advance(60_000);
    assert!(w.texts(B).is_empty());
    assert!(w.texts(C).is_empty());
    assert_eq!(w.texts(D), ["to D"]);
    assert!(rows::outbox(&w, A).iter().all(|row| row.peer == d));
}

/// What the server took for a device is not sent to it again after a
/// restart: the outbox keeps that it waits for a receipt.
#[test]
fn a_message_the_server_took_is_not_sent_again_after_a_restart() {
    let mut w = settled();
    w.disconnect(B);
    let id = w.send_text(A, B, "taken while you were away");
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Queued));
    let a = addr(&w, A);
    assert_eq!(inbox_items_from(&w, B, 0, a), 1);

    w.restart(A);
    w.advance(10 * 60_000);
    assert_eq!(inbox_items_from(&w, B, 0, a), 1, "not left there twice");
    w.connect(B);
    w.advance(1_000);
    assert_eq!(w.texts(B), ["taken while you were away"]);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
}

/// The ids of client `i`'s outbox rows for `peer`'s client.
fn waiting_for(w: &World, i: usize, peer: usize) -> HashSet<MsgId> {
    let peer = w.peer(peer);
    rows::outbox(w, i)
        .into_iter()
        .filter(|row| row.peer == peer)
        .map(|row| row.msg_id)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 3,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// At most 500 messages wait for one contact's receipts: beyond that
    /// the oldest are let go. Messages waiting for another contact stay.
    #[test]
    fn at_most_500_messages_wait_for_one_contact(extra in 1usize..12) {
        let mut w = World::new(3);
        w.pair(A, B);
        w.pair(A, C);
        w.disconnect(B);
        w.disconnect(C);
        let to_c: HashSet<MsgId> = (0..3)
            .map(|_| {
                w.now += 1;
                w.send_text(A, C, "to C")
            })
            .collect();
        let to_b: Vec<MsgId> = (0..500 + extra)
            .map(|n| {
                w.now += 1;
                w.send_text(A, B, &format!("to B {n}"))
            })
            .collect();
        let kept = waiting_for(&w, A, B);
        prop_assert_eq!(kept.len(), 500);
        prop_assert!(to_b.iter().skip(extra).all(|id| kept.contains(id)), "the newest stay");
        prop_assert_eq!(waiting_for(&w, A, C), to_c);
    }
}

/// When too many wait, those the contact already read go first: before
/// older ones only delivered, and before any still on its way.
#[test]
fn read_messages_are_let_go_first() {
    let mut w = paired();
    let a = w.peer(A);
    let sent: Vec<MsgId> = (0..500)
        .map(|n| w.send_text(A, B, &n.to_string()))
        .collect();
    let read: Vec<MsgId> = sent.iter().rev().take(10).copied().collect();
    w.command(
        B,
        Command::MarkRead {
            peer: a,
            ids: read.clone(),
        },
    );
    w.disconnect(B);
    let queued = w.send_text(A, B, "on its way");
    let kept = waiting_for(&w, A, B);
    assert_eq!(kept.len(), 500);
    assert!(kept.contains(&queued));
    assert_eq!(read.iter().filter(|id| !kept.contains(id)).count(), 1);
}
