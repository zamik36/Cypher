//! Messages that come again: an exact copy of one already read is no news
//! and changes nothing, while one altered on the way, or never read, that
//! does not decrypt still warns and starts the session afresh.

use bytes::Bytes;
use cypher_core::relay::RelayBody;
use cypher_core::{Event, FailReason};
use cypher_types::Addr;
use cypher_wire::{ClientMsg, Frame, ServerMsg};

use super::{addr, events_since, is_message, open_item};
use crate::harness::{Seen, World};
use crate::{A, B, paired};

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
