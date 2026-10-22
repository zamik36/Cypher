//! Messages that come again: an exact copy of one already read is no news
//! and changes nothing, while one altered on the way, or never read, that
//! does not decrypt still warns and starts the session afresh.

use bytes::Bytes;
use cypher_core::relay::{self, RelayBody};
use cypher_core::{Command, Event, FailReason, Table, session_key};
use cypher_crypto::{IdentitySeed, InitHeader, handshake};
use cypher_types::{Addr, DeviceId};
use cypher_wire::{ClientMsg, Frame, ServerMsg};
use rand::rngs::OsRng;

use super::{addr, events_since, is_message, open_item, rows};
use crate::harness::{Fault, Rule, Seen, World};
use crate::{A, B, C, paired};

/// A world where the two have talked back and forth, each turn on a new
/// ratchet chain, so that the frames delivered so far span chains long
/// left behind as well as the current one.
fn talked() -> World {
    let mut w = paired();
    for turn in ["one", "two", "three"] {
        w.send_text(A, B, turn);
        w.send_text(B, A, turn);
    }
    w
}

/// Whether client `i` was warned that a message did not decrypt.
fn warned(w: &World, i: usize, from: usize) -> bool {
    events_since(w, i, from).iter().any(|e| {
        matches!(
            e,
            Event::Warning {
                reason: FailReason::DecryptFailed
            }
        )
    })
}

/// The end-to-end messages client `i` sent to client `to` since the log
/// was started, over the session or sealed through the relay, as `to`
/// would get them.
fn sent_toward(w: &World, i: usize, to: usize) -> Vec<Bytes> {
    let target = addr(w, to);
    let log = w.log.as_deref().unwrap_or_default();
    log.iter()
        .filter(|batch| batch.client == i)
        .flat_map(|batch| &batch.effects)
        .filter_map(|seen| match seen {
            Seen::Transmit(raw) => match Frame::<ClientMsg>::decode(raw.clone()).ok()?.msg {
                ClientMsg::Send {
                    to: peer,
                    device,
                    body,
                    ..
                } if Addr::new(peer, device) == target => Some(body),
                _ => None,
            },
            Seen::Anonymous(blob) => match w.open_anonymous(blob)?.msg {
                ClientMsg::SendSealed {
                    to: peer,
                    device,
                    item,
                    ..
                } if Addr::new(peer, device) == target => {
                    open_item(w, to, &item).map(|(_, body)| body)
                }
                _ => None,
            },
            _ => None,
        })
        .filter(is_message)
        .collect()
}

/// A frame that hands `body` to client `to` from `from` over the session.
fn over_session(from: Addr, body: Bytes) -> Bytes {
    Frame::new(
        0,
        ServerMsg::Recv {
            from: from.peer,
            device: from.device,
            body,
        },
    )
    .encode()
}

/// The body of the last end-to-end message client `i` was handed by the
/// server.
fn last_message_from(w: &World, i: usize) -> Bytes {
    let peer = w.peer(i);
    crate::delivered_bodies(w)
        .into_iter()
        .filter_map(|(from, body)| (from == peer).then_some(body))
        .filter(is_message)
        .last()
        .expect("a message delivered")
}

/// `body` with the last byte of its ciphertext (the AEAD tag) flipped.
fn altered(body: &Bytes) -> Bytes {
    let mut raw = body.to_vec();
    if let Some(last) = raw.last_mut() {
        *last ^= 1;
    }
    Bytes::from(raw)
}

/// The server hands every frame it delivered so far to its recipient once
/// more: the inits that started the session, greetings, receipts and texts,
/// over the session and sealed, from chains long left behind and from the
/// current one. Neither side hears of it, nor answers, nor fetches keys,
/// and the two go on talking both ways.
#[test]
fn copies_of_messages_already_read_are_no_news() {
    let mut w = talked();
    let delivered = w.server.delivered.clone();
    let inits = delivered
        .iter()
        .filter(|(_, raw)| {
            matches!(
                Frame::<ServerMsg>::decode(raw.clone()).map(|f| f.msg),
                Ok(ServerMsg::Recv { body, .. })
                    if matches!(RelayBody::decode(&body), Ok(RelayBody::Message { init: Some(_), .. }))
            )
        })
        .count();
    assert!(inits > 0, "the inits are among the copies");
    let events = [w.clients[A].events.len(), w.clients[B].events.len()];
    let (fetches, transmitted) = (w.server.key_fetches, w.transmitted.len());

    for (to, raw) in delivered {
        w.inject(to, raw);
    }

    for (i, from) in [A, B].into_iter().zip(events) {
        assert_eq!(events_since(&w, i, from), [], "client {i} told nothing");
    }
    assert_eq!(w.server.key_fetches, fetches, "no session started again");
    assert_eq!(w.transmitted.len(), transmitted, "nothing answered");

    w.send_text(A, B, "still here");
    w.send_text(B, A, "me too");
    assert_eq!(w.texts(B), ["one", "two", "three", "still here"]);
    assert_eq!(w.texts(A), ["one", "two", "three", "me too"]);
    for (i, from) in [A, B].into_iter().zip(events) {
        assert!(!warned(&w, i, from), "client {i}");
    }
}

/// B reads an inbox item from A, but the server never hears B's ack; then
/// B's session with A is replaced by a fresh one. The item handed out again
/// is a copy of one read in the old session: no news either.
#[test]
fn copies_from_a_session_since_replaced_are_no_news() {
    let mut w = paired();
    w.disconnect(B);
    w.send_text(A, B, "while you were away");
    w.server.faults.push(Rule::always(
        |m| matches!(m, ClientMsg::InboxAck { .. }),
        Fault::Ignore,
    ));
    w.connect(B);
    w.run();
    assert_eq!(w.texts(B), ["while you were away"]);
    let (a, last) = (addr(&w, A), last_message_from(&w, A));
    let fetches = w.server.key_fetches;
    w.inject(B, over_session(a, altered(&last)));
    assert!(w.server.key_fetches > fetches, "a fresh session");

    let (from, fetches) = (w.clients[B].events.len(), w.server.key_fetches);
    let reads = |w: &World| {
        w.server
            .served
            .iter()
            .filter(|(i, _, m)| *i == B && matches!(m, ClientMsg::InboxFetch { .. }))
            .count()
    };
    let before = reads(&w);
    w.advance(10 * 60_000);
    assert!(reads(&w) > before, "the inbox was read again");

    assert!(!warned(&w, B, from));
    assert_eq!(w.server.key_fetches, fetches, "no session started again");
    assert_eq!(w.texts(B), ["while you were away"]);
}

/// A copy of a message already read, altered on the way, is no copy: it
/// fails like any message that does not decrypt.
#[test]
fn an_altered_copy_warns_and_starts_afresh() {
    let mut w = talked();
    let (a, last) = (addr(&w, A), last_message_from(&w, A));
    let (from, fetches) = (w.clients[B].events.len(), w.server.key_fetches);

    w.inject(B, over_session(a, altered(&last)));

    assert!(warned(&w, B, from));
    assert!(w.server.key_fetches > fetches, "a session started afresh");
}

/// The server keeps a message to B and later hands it over altered: never
/// read, it warns and B starts the session afresh, on which the message
/// comes again. Untouched, the same message is read as usual.
#[test]
fn a_fresh_message_altered_on_the_way_warns_and_starts_afresh() {
    let kept = || {
        let mut w = talked();
        w.lose_messages_from(A, 1);
        w.log = Some(Vec::new());
        w.send_text(A, B, "kept by the server");
        let body = sent_toward(&w, A, B).pop().expect("the message, sent once");
        (w, body)
    };

    let (mut w, body) = kept();
    let from = w.clients[B].events.len();
    let a = addr(&w, A);
    w.inject(B, over_session(a, body));
    assert_eq!(
        w.texts(B).last().map(String::as_str),
        Some("kept by the server")
    );
    assert!(!warned(&w, B, from));

    let (mut w, body) = kept();
    let (from, fetches) = (w.clients[B].events.len(), w.server.key_fetches);
    let a = addr(&w, A);
    w.inject(B, over_session(a, altered(&body)));
    assert!(warned(&w, B, from));
    assert!(w.server.key_fetches > fetches, "a session started afresh");
    let shown = w.texts(B);
    let kept = shown.iter().filter(|t| *t == "kept by the server");
    assert_eq!(kept.count(), 1, "sent again on the fresh session");
}

/// Which messages a session read is kept in memory only: once the app
/// restarts, a copy of one read before fails like a message whose key is
/// spent, and the session starts afresh.
#[test]
fn after_a_restart_a_copy_warns_and_starts_afresh() {
    let mut w = talked();
    let (a, last) = (addr(&w, A), last_message_from(&w, A));
    w.restart(B);
    let (from, fetches) = (w.clients[B].events.len(), w.server.key_fetches);

    w.inject(B, over_session(a, last));

    assert!(warned(&w, B, from));
    assert!(w.server.key_fetches > fetches, "a session started afresh");
}

/// The init a frame delivered to client `to` carries, if any.
fn init_in(w: &World, to: usize, raw: &Bytes) -> Option<InitHeader> {
    let body = match Frame::<ServerMsg>::decode(raw.clone()).ok()?.msg {
        ServerMsg::Recv { body, .. } => body,
        ServerMsg::RecvSealed { item } => open_item(w, to, &item)?.1,
        _ => return None,
    };
    match RelayBody::decode(&body).ok()? {
        RelayBody::Message { init, .. } => init,
        _ => None,
    }
}

/// B's session with A fails to decrypt, so B starts a fresh one; a while
/// later, so that the next may start too. The frame with B's init that
/// reached A, and that init.
fn restart_from_b(w: &mut World) -> (Bytes, InitHeader) {
    let a = addr(w, A);
    let (start, fetches) = (w.server.delivered.len(), w.server.key_fetches);
    let last = last_message_from(w, A);
    w.inject(B, over_session(a, altered(&last)));
    assert!(w.server.key_fetches > fetches, "B starts a fresh session");
    let init = w.server.delivered[start..]
        .iter()
        .find_map(|(to, raw)| {
            (*to == A)
                .then(|| init_in(w, A, raw))
                .flatten()
                .map(|init| (raw.clone(), init))
        })
        .expect("an init reached A");
    w.advance(10 * 60_000);
    init
}

/// Hands `frame` to client `to`, which must change nothing: its session
/// with `with` stays as it is, and it neither answers nor tells.
fn refused_by(w: &mut World, to: usize, with: Addr, frame: Bytes) {
    let row = (Table::Sessions as u8, session_key(with));
    let session = w.clients[to].kv[&row].clone();
    let events = w.clients[to].events.len();
    let sent = |w: &World| w.transmitted.iter().filter(|(i, _)| *i == to).count();
    let transmitted = sent(w);
    w.inject(to, frame);

    assert!(w.clients[to].kv[&row] == session, "the session stays");
    assert_eq!(sent(w), transmitted, "nothing answered");
    assert_eq!(events_since(w, to, events), [], "nothing told");
}

/// B starts seventeen sessions with A in turn, each from A's signed
/// prekey alone, so that A's session with B no longer remembers the first
/// init among the inits it accepted. That init, handed to A again, is
/// refused: A's session stays as it is, and A neither answers nor tells.
#[test]
fn an_init_replayed_after_its_session_forgot_it_is_refused() {
    let mut w = paired();
    w.exhaust_opks(A);
    let b = addr(&w, B);
    let (frame, init) = restart_from_b(&mut w);
    assert_eq!(init.opk_id, None, "from the signed prekey alone");
    for _ in 0..16 {
        restart_from_b(&mut w);
    }
    let remembered = rows::session(&w, A, b).unwrap().accepted_ephemerals;
    assert!(
        !remembered.contains(&init.ephemeral),
        "the session forgot it"
    );
    refused_by(&mut w, A, b, frame);
}

/// A was updated from a version whose prekeys did not remember the inits
/// accepted. The inits its sessions remember are taken as accepted, so one
/// of them handed to A again, once its session has forgotten it, is still
/// refused.
#[test]
fn inits_accepted_before_the_update_stay_refused() {
    let mut w = paired();
    w.exhaust_opks(A);
    let b = addr(&w, B);
    let (frame, init) = restart_from_b(&mut w);
    rows::store_prekeys_as_v1(&mut w, A);
    w.restart(A);
    for _ in 0..16 {
        restart_from_b(&mut w);
    }
    let remembered = rows::session(&w, A, b).unwrap().accepted_ephemerals;
    assert!(
        !remembered.contains(&init.ephemeral),
        "the session forgot it"
    );
    refused_by(&mut w, A, b, frame);
}

/// An init a stranger C makes from A's published keys, starting a session
/// afresh, as a frame A gets from C; its ciphertext altered if `tampered`.
fn init_from_stranger(w: &World, tampered: bool) -> Bytes {
    let identity = IdentitySeed(w.clients[C].seed).derive_identity();
    let bundle = w.bundle_of(addr(w, A));
    let (mut ratchet, init) =
        handshake::initiate(&identity, DeviceId::FIRST, &bundle, &mut OsRng).unwrap();
    let (header, ct) = ratchet.encrypt(b"let me in", b"").unwrap();
    let body = Bytes::from(relay::message_body(Some(&init), &header, &ct));
    let body = if tampered { altered(&body) } else { body };
    over_session(Addr::new(identity.peer_id(), DeviceId::FIRST), body)
}

/// How many times client `i` was told an init was turned away.
fn turned_away(w: &World, i: usize) -> usize {
    w.clients[i]
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::Warning {
                    reason: FailReason::Rejected
                }
            )
        })
        .count()
}

/// A stranger floods A with inits, each validly made from A's keys. Only
/// so many are taken from one stranger; A says it turns the next away.
/// A contact whose session needs starting again is not held back.
#[test]
fn a_stranger_flooding_inits_does_not_lock_out_contacts() {
    let mut w = World::new(3);
    w.pair(A, B);
    for n in 0..33 {
        let frame = init_from_stranger(&w, false);
        w.inject(A, frame);
        assert_eq!(turned_away(&w, A), usize::from(n >= 32), "init {n}");
    }

    let from = w.clients[A].events.len();
    restart_from_b(&mut w);
    w.send_text(A, B, "after the flood");
    w.send_text(B, A, "still here");
    assert_eq!(
        w.texts(B).last().map(String::as_str),
        Some("after the flood")
    );
    assert_eq!(w.texts(A).last().map(String::as_str), Some("still here"));
    assert!(!warned(&w, A, from));
}

/// Both join each other's invite at once, each starting a session: the
/// lower device keeps its own and turns the other's init away. Once the
/// two have talked on the kept session, that init handed to the lower
/// device again is refused as well, and the session stays.
#[test]
fn the_init_turned_away_when_both_joined_at_once_stays_refused() {
    let mut w = World::new(2);
    let link_a = w.create_link(A);
    let link_b = w.create_link(B);
    w.queue(B, Command::JoinLink { link: link_a });
    w.queue(A, Command::JoinLink { link: link_b });
    w.run();
    let (low, high) = if (w.peer(A), w.clients[A].device) < (w.peer(B), w.clients[B].device) {
        (A, B)
    } else {
        (B, A)
    };
    let turned_away = w
        .server
        .delivered
        .iter()
        .find(|(to, raw)| *to == low && init_in(&w, low, raw).is_some())
        .map(|(_, raw)| raw.clone())
        .expect("the higher device's init");
    w.send_text(A, B, "ping");
    w.send_text(B, A, "pong");
    assert_eq!(w.texts(B), ["ping"]);
    assert_eq!(w.texts(A), ["pong"]);

    let high_addr = addr(&w, high);
    refused_by(&mut w, low, high_addr, turned_away);
}

/// Once a stranger is beyond its quota, A says so once. Inits that do not
/// even decrypt, made by anyone in the stranger's name, are not turned
/// away for the quota: they fail as any message that does not decrypt.
#[test]
fn turning_a_stranger_away_is_told_once_and_only_for_real_inits() {
    let mut w = World::new(3);
    w.pair(A, B);
    for _ in 0..32 {
        let frame = init_from_stranger(&w, false);
        w.inject(A, frame);
    }
    assert_eq!(turned_away(&w, A), 0);
    for tampered in [true, true, false, true, false] {
        let frame = init_from_stranger(&w, tampered);
        w.inject(A, frame);
    }
    assert_eq!(turned_away(&w, A), 1);
}
