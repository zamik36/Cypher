//! Who becomes a contact and how: invites, strangers waiting for an
//! answer, joining a host with several devices, both sides joining at
//! once; and what a contact can be sent, and forgetting one.

use cypher_core::envelope::MAX_TEXT_LEN;
use cypher_core::{Command, Event, FailReason, MessageStatus};
use cypher_crypto::InitHeader;
use cypher_types::{Addr, DeviceId, PeerId};

use super::{addr, rows};
use crate::harness::World;
use crate::{A, B, C, D, delivered_bodies, drift, paired};

/// How long an invite admits a contact.
const DAY_MS: u64 = 24 * 3600 * 1000;

/// `joiner` is a contact of `host`, which said so.
fn admitted(w: &World, host: usize, joiner: usize) -> bool {
    let peer = w.peer(joiner);
    w.has_event(
        host,
        |e| matches!(e, Event::PeerAdded { peer: p, .. } if *p == peer),
    ) && !w.contact(host, peer).request
}

/// `joiner` waits for `host` to answer.
fn waits(w: &World, host: usize, joiner: usize) -> bool {
    let peer = w.peer(joiner);
    w.has_session(host, peer) && w.contact(host, peer).request
}

/// The invite's code, as the host keeps it.
fn code(share: &str) -> String {
    share
        .split_once('-')
        .map_or(share, |(code, _)| code)
        .to_owned()
}

#[test]
fn an_invite_admits_for_a_day() {
    let mut w = World::new(3);
    let link = w.create_link(A);
    w.now += DAY_MS - 1;
    w.command(B, Command::JoinLink { link });
    assert!(admitted(&w, A, B), "a moment before the day is over");

    let link = w.create_link(A);
    w.now += DAY_MS;
    w.command(C, Command::JoinLink { link });
    assert!(waits(&w, A, C), "a day old: only a request");
}

#[test]
fn an_invite_admits_one_contact_and_leaves_the_others_valid() {
    let mut w = World::new(4);
    let first = w.create_link(A);
    let second = w.create_link(A);
    w.command(
        B,
        Command::JoinLink {
            link: first.clone(),
        },
    );
    w.command(C, Command::JoinLink { link: first });
    w.command(D, Command::JoinLink { link: second });
    assert!(admitted(&w, A, B));
    assert!(waits(&w, A, C), "used once already");
    assert!(admitted(&w, A, D), "another invite still admits");
}

/// The newest 32 invites are kept; making one more forgets the oldest.
#[test]
fn of_more_than_32_invites_the_oldest_is_forgotten() {
    for (made, oldest_admits) in [(32, true), (33, false)] {
        let mut w = World::new(3);
        let links: Vec<String> = (0..made).map(|_| w.create_link(A)).collect();
        w.command(
            B,
            Command::JoinLink {
                link: links[0].clone(),
            },
        );
        assert_eq!(admitted(&w, A, B), oldest_admits, "of {made} invites");
        w.command(
            C,
            Command::JoinLink {
                link: links[1].clone(),
            },
        );
        assert!(admitted(&w, A, C), "the second of {made} admits");
    }
}

#[test]
fn an_invite_survives_a_restart() {
    let mut w = World::new(2);
    let link = w.create_link(A);
    w.restart(A);
    w.command(B, Command::JoinLink { link });
    assert!(admitted(&w, A, B));
}

/// Invites a day old are not kept: making a new one forgets them.
#[test]
fn invites_a_day_old_are_not_kept() {
    let mut w = World::new(1);
    w.create_link(A);
    w.now += DAY_MS;
    let new = w.create_link(A);
    assert_eq!(rows::invites(&w, A), [code(&new)]);
}

/// How many strangers client `i` was told wait for an answer.
fn requests(w: &World, i: usize) -> usize {
    w.clients[i]
        .events
        .iter()
        .filter(|e| matches!(e, Event::ContactRequest { .. }))
        .count()
}

/// At most twenty strangers wait for an answer; one more is not let in,
/// quietly, until one is answered.
#[test]
fn at_most_twenty_strangers_wait_for_an_answer() {
    const WAITING: usize = 20;
    let late = WAITING + 2;
    let mut w = World::new(late + 1);
    let link = w.create_link(A);
    w.command(B, Command::JoinLink { link: link.clone() });
    for i in 2..late {
        w.command(i, Command::JoinLink { link: link.clone() });
    }
    assert_eq!(requests(&w, A), WAITING);
    w.command(late, Command::JoinLink { link });
    assert!(!w.has_session(A, w.peer(late)), "one too many");
    assert_eq!(requests(&w, A), WAITING);
    assert!(!w.has_event(A, |e| matches!(e, Event::Warning { .. })));

    let first = w.peer(2);
    w.command(A, Command::AcceptContact { peer: first });
    w.send_text(late, A, "room now?");
    assert!(waits(&w, A, late));
    assert!(w.texts(A).contains(&"room now?".to_owned()));
}

/// With twenty strangers waiting, a contact whose session had to start
/// afresh and a new device of this user still get through.
#[test]
fn contacts_and_own_devices_get_through_while_strangers_wait() {
    let mut w = World::new(22);
    let link = w.create_link(A);
    w.command(B, Command::JoinLink { link: link.clone() });
    for i in 2..22 {
        w.command(i, Command::JoinLink { link: link.clone() });
    }
    assert_eq!(requests(&w, A), 20);
    w.send_text(A, B, "one");
    w.send_text(B, A, "two");
    let older = w.clients[A].kv.clone();
    w.send_text(A, B, "three");
    w.send_text(B, A, "four");
    // A writes from the past: B cannot read it and starts afresh.
    drift(&mut w, A, older, B);
    w.send_text(B, A, "after the repair");
    assert_eq!(w.texts(A).last().unwrap(), "after the repair");

    let a2 = w.add_device(A, 2);
    w.advance(60 * 60_000);
    let b = w.peer(B);
    assert!(!w.contact(a2, b).request, "the sibling told it of B");
    w.send_text(B, A, "to both");
    assert_eq!(w.texts(a2).last().unwrap(), "to both");
}

/// A host with a second device, both with their keys published.
fn host_with_two_devices() -> World {
    let mut w = World::new(2);
    w.add_device(B, 2);
    w.advance(60 * 60_000);
    w
}

/// How joining went for client `i`: how often it was told the contact is
/// there, and the reasons it was told it failed.
fn join_outcome(w: &World, i: usize) -> (usize, Vec<FailReason>) {
    let events = &w.clients[i].events;
    let added = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::PeerAdded {
                    initiated_by_us: true,
                    ..
                }
            )
        })
        .count();
    let failed = events
        .iter()
        .filter_map(|e| match e {
            Event::JoinFailed { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect();
    (added, failed)
}

/// Joining works, and is told once, when any device of the host can be
/// reached, whichever answers first.
#[test]
fn joining_succeeds_once_when_any_device_of_the_host_answers() {
    for missing in [1, 2] {
        let mut w = host_with_two_devices();
        let b = w.peer(B);
        w.remove_keys(Addr::new(b, DeviceId(missing)));
        let link = w.create_link(B);
        w.command(A, Command::JoinLink { link });
        assert_eq!(
            join_outcome(&w, A),
            (1, Vec::new()),
            "device {missing} has no keys"
        );
    }
}

/// When no device of the host can be reached, joining fails once, with the
/// reason the last one gave.
#[test]
fn joining_fails_once_when_no_device_of_the_host_answers() {
    let mut w = host_with_two_devices();
    let b = w.peer(B);
    w.remove_keys(Addr::new(b, DeviceId(1)));
    w.remove_keys(Addr::new(b, DeviceId(2)));
    let link = w.create_link(B);
    w.command(A, Command::JoinLink { link });
    assert_eq!(join_outcome(&w, A), (0, vec![FailReason::NotFound]));
}

/// Both join each other's invite at the same moment, each starting a
/// session: the device first in order (identity, then device id) keeps the
/// one it started, the other takes it in place of its own. Nothing fails to
/// decrypt and no session is started again.
#[test]
fn when_both_join_at_once_the_lower_device_keeps_its_session() {
    let mut w = World::new(2);
    let link_a = w.create_link(A);
    let link_b = w.create_link(B);
    w.queue(B, Command::JoinLink { link: link_a });
    w.queue(A, Command::JoinLink { link: link_b });
    w.run();
    let fetches = w.server.key_fetches;
    w.send_text(A, B, "ping");
    w.send_text(B, A, "pong");
    assert_eq!(w.texts(B), ["ping"]);
    assert_eq!(w.texts(A), ["pong"]);
    assert_eq!(w.server.key_fetches, fetches, "no session started again");
    for i in [A, B] {
        assert!(!w.has_event(i, |e| matches!(
            e,
            Event::Warning {
                reason: FailReason::DecryptFailed
            }
        )));
    }

    let (low, high) = if (w.peer(A), w.clients[A].device) < (w.peer(B), w.clients[B].device) {
        (A, B)
    } else {
        (B, A)
    };
    let (low_addr, high_addr) = (addr(&w, low), addr(&w, high));
    let kept = rows::session(&w, low, high_addr).unwrap();
    let taken = rows::session(&w, high, low_addr).unwrap();
    assert!(kept.accepted_ephemerals.is_empty(), "kept its own");
    // The init the lower device sent is the one the higher took.
    let low_peer = w.peer(low);
    let ephemerals: Vec<[u8; 32]> = delivered_bodies(&w)
        .into_iter()
        .filter(|(from, body)| *from == low_peer && body.get(1) == Some(&1))
        .map(|(_, body)| InitHeader::decode_prefix(&body[2..]).unwrap().0.ephemeral)
        .collect();
    assert!(!ephemerals.is_empty(), "the lower device sent its init");
    assert_eq!(taken.accepted_ephemerals, ephemerals[..1]);
}

/// A text is sent only when it is not empty, not longer than the limit,
/// and to a contact; otherwise it fails at once.
#[test]
fn texts_go_only_within_the_limit_and_to_contacts() {
    let mut w = paired();
    let longest = "x".repeat(MAX_TEXT_LEN);
    let sent = w.send_text(A, B, &longest);
    assert_eq!(w.texts(B), [longest]);
    assert_eq!(w.status_of(A, sent), Some(MessageStatus::Delivered));

    let stranger = PeerId([9; 32]);
    for (text, peer) in [
        (String::new(), w.peer(B)),
        ("x".repeat(MAX_TEXT_LEN + 1), w.peer(B)),
        ("hi".to_owned(), stranger),
    ] {
        let msg_id = w.msg_id();
        w.command(
            A,
            Command::SendText {
                peer,
                msg_id,
                text,
                reply_to: None,
            },
        );
        assert_eq!(w.status_of(A, msg_id), Some(MessageStatus::Failed));
        assert!(!w.has_event(A, |e| matches!(e, Event::Message(m) if m.msg_id == msg_id)));
    }
    assert_eq!(w.texts(B).len(), 1, "nothing else went");
}

/// Forgetting a contact leaves the sessions with the others alone.
#[test]
fn forgetting_a_contact_leaves_the_others() {
    let mut w = World::new(3);
    w.pair(A, B);
    w.pair(A, C);
    let (b, c) = (w.peer(B), w.peer(C));
    let to_c = addr(&w, C);
    w.command(A, Command::RemovePeer { peer: b });
    assert!(!w.has_session(A, b));
    assert!(rows::session(&w, A, addr(&w, B)).is_none());
    assert!(w.has_session(A, c) && rows::session(&w, A, to_c).is_some());
    w.send_text(A, C, "still here");
    assert_eq!(w.texts(C), ["still here"]);
}

/// When a contact's message says it was sent: a clock a little ahead of
/// ours is believed, one further ahead is held to ten minutes past ours.
#[test]
fn a_date_ahead_of_ours_is_held_to_ten_minutes() {
    let mut w = paired();
    for (ahead, dated) in [(5 * 60_000, 5 * 60_000), (30 * 60_000, 10 * 60_000)] {
        w.clients[B].ahead = ahead;
        let id = w.send_text(B, A, "from ahead");
        w.clients[B].ahead = 0;
        let sent_at = w.clients[A].events.iter().find_map(|e| match e {
            Event::Message(m) if m.msg_id == id => Some(m.sent_at_ms),
            _ => None,
        });
        assert_eq!(sent_at, Some(w.now + dated), "{ahead} ms ahead");
    }
}

/// A new name reaches the contacts already greeted, as news of a name,
/// not as a request; a contact this side blocked is not told.
#[test]
fn a_new_name_skips_blocked_contacts_and_is_no_request() {
    let mut w = World::new(3);
    w.pair(A, B);
    w.pair(A, C);
    let (a, c) = (w.peer(A), w.peer(C));
    w.command(A, Command::BlockPeer { peer: c });
    w.command(
        A,
        Command::SetProfileName {
            name: Some("Alice".into()),
        },
    );
    assert_eq!(w.contact_name(B, a).as_deref(), Some("Alice"));
    assert!(!w.has_event(B, |e| matches!(e, Event::ContactRequest { .. })));
    assert_eq!(w.contact_name(C, a), None, "blocked, so not told");
}

/// A message from a device the contact's list has does not have their
/// devices looked up again.
#[test]
fn a_message_from_a_listed_device_looks_nothing_up() {
    let mut w = paired();
    w.advance(60 * 60_000);
    let fetches = w.server.device_fetches;
    w.send_text(B, A, "from a device you know");
    w.send_text(A, B, "and back");
    assert_eq!(w.server.device_fetches, fetches);
}
