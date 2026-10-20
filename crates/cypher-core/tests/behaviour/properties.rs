//! Properties over random runs: two contacts write while they go offline,
//! restart, lose their relay and time passes, in any order.

use std::collections::HashSet;

use bytes::Bytes;
use cypher_core::{Core, Input, Table, session_key};
use cypher_crypto::IdentitySeed;
use cypher_types::{Addr, MsgId, PeerId};
use cypher_wire::{ClientMsg, Frame};
use proptest::prelude::*;
use rand::SeedableRng;

use super::{is_message, open_item};
use crate::harness::{Rng, Seen, World};
use crate::{A, B, paired};

#[derive(Debug, Clone)]
enum Step {
    Write(usize),
    Disconnect(usize),
    Connect(usize),
    Restart(usize),
    Relay(usize, bool),
    Wait(u64),
}

fn step() -> impl Strategy<Value = Step> {
    let client = 0..2usize;
    prop_oneof![
        3 => client.clone().prop_map(Step::Write),
        1 => client.clone().prop_map(Step::Disconnect),
        1 => client.clone().prop_map(Step::Connect),
        1 => client.clone().prop_map(Step::Restart),
        1 => (client, any::<bool>()).prop_map(|(i, up)| Step::Relay(i, up)),
        2 => prop_oneof![Just(500u64), Just(5_000), Just(60_000)].prop_map(Step::Wait),
    ]
}

/// The world after `steps`, with every client back online and its relay
/// up long enough for whatever waited to go; and what was written, by
/// whom to whom.
fn play(steps: &[Step]) -> (World, Vec<(usize, usize, MsgId)>) {
    let mut w = paired();
    w.log = Some(Vec::new());
    let mut relay = [true, true];
    let mut written = Vec::new();
    for (n, step) in steps.iter().enumerate() {
        match *step {
            Step::Write(i) => {
                let to = 1 - i;
                written.push((i, to, w.send_text(i, to, &format!("message {n}"))));
            }
            Step::Disconnect(i) if w.clients[i].connected => w.disconnect(i),
            Step::Connect(i) if !w.clients[i].connected => {
                w.relay_up = relay[i];
                w.connect(i);
                w.run();
            }
            Step::Restart(i) => {
                w.relay_up = relay[i];
                w.restart(i);
            }
            Step::Relay(i, up) => {
                relay[i] = up;
                if w.clients[i].connected {
                    w.set_relay(i, up);
                }
            }
            Step::Wait(ms) => w.advance(ms),
            Step::Disconnect(_) | Step::Connect(_) => {}
        }
    }
    w.relay_up = true;
    for i in [A, B] {
        if w.clients[i].connected {
            w.set_relay(i, true);
        } else {
            w.connect(i);
            w.run();
        }
    }
    w.advance(20 * 60_000);
    (w, written)
}

/// The clients that are `peer`, or all of them for `None`.
fn clients(w: &World, peer: Option<PeerId>) -> impl Iterator<Item = usize> + '_ {
    (0..w.clients.len()).filter(move |&i| {
        let id = w.clients[i].core.as_ref().map(Core::peer_id);
        id.is_some() && peer.is_none_or(|peer| id == Some(peer))
    })
}

/// The body sealed in `item` for `to` (any client for `None`), opened as
/// its recipient does.
fn open_for(w: &World, to: Option<PeerId>, item: &[u8]) -> Option<Bytes> {
    clients(w, to).find_map(|i| open_item(w, i, item).map(|(_, body)| body))
}

/// What leaves a core carrying an end-to-end message, over the session or
/// through the relay alike: to whom (`None`: left in an inbox, whoever
/// reads it) and the message.
fn carried(w: &World, effect: &Seen) -> Result<Option<(Option<Addr>, Bytes)>, String> {
    let msg = match effect {
        Seen::Transmit(raw) => Frame::<ClientMsg>::decode(raw.clone()).ok().map(|f| f.msg),
        Seen::Anonymous(blob) => w.open_anonymous(blob).map(|f| f.msg),
        Seen::Persist { .. } | Seen::Emit(_) | Seen::ReadChunk(_) | Seen::Other => None,
    };
    Ok(match msg {
        Some(ClientMsg::Send {
            to, device, body, ..
        }) if is_message(&body) && clients(w, Some(to)).next().is_some() => {
            Some((Some(Addr::new(to, device)), body))
        }
        Some(ClientMsg::SendSealed {
            to, device, item, ..
        }) => {
            let body = open_for(w, Some(to), &item).ok_or("a sealed send no one opens")?;
            Some((Some(Addr::new(to, device)), body))
        }
        Some(ClientMsg::InboxPut { item, .. }) => Some((
            None,
            open_for(w, None, &item).ok_or("an item no one opens")?,
        )),
        _ => None,
    })
}

/// Every end-to-end message a core hands out for the first time comes
/// after its session's new state, in the same batch: should the driver
/// stop in between, the next start cannot encrypt under that message key
/// again. What is left in an inbox was sent before, as it is.
fn check_persist_before_send(w: &World) -> Result<(), String> {
    let mut sent: HashSet<(usize, Bytes)> = HashSet::new();
    for batch in w.log.as_ref().expect("the log is kept") {
        let mut stored: HashSet<&[u8]> = HashSet::new();
        for effect in &batch.effects {
            if let Seen::Persist { table, key } = effect
                && *table == Table::Sessions as u8
            {
                stored.insert(key);
            }
            let Some((to, body)) = carried(w, effect)? else {
                continue;
            };
            let fresh = sent.insert((batch.client, body));
            match to {
                None if fresh => return Err(format!("an inbox item never sent: {batch:?}")),
                Some(to) if fresh && !stored.contains(session_key(to).as_slice()) => {
                    return Err(format!("sent to {to:?} before its session was stored"));
                }
                None | Some(_) => {}
            }
        }
    }
    Ok(())
}

/// A core restored twice from the same rows, with the same randomness,
/// makes the same effects then, and on connecting.
fn check_restore_is_deterministic(w: &World, i: usize) -> Result<(), String> {
    let snapshot = w.snapshot(i);
    let c = &w.clients[i];
    let once = || {
        let rng = Rng::seed_from_u64(9);
        let (mut core, restored) =
            Core::restore(&IdentitySeed(c.seed), c.device, &snapshot, w.now, rng).unwrap();
        let connected = core.handle(Input::Connected, w.now);
        format!("{restored:?} {connected:?}")
    };
    if once() == once() {
        Ok(())
    } else {
        Err(format!("client {i} restores differently"))
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: if cfg!(miri) { 1 } else { 24 },
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Whatever happens in between, every message written reaches its
    /// contact and is stored there once; every message sent comes after
    /// its session's state; a core starts alike from alike rows.
    #[test]
    fn messages_arrive_once_and_follow_their_stored_sessions(
        steps in prop::collection::vec(step(), 1..24)
    ) {
        let (w, written) = play(&steps);
        for (from, to, id) in &written {
            prop_assert_eq!(
                w.stored_copies(*to, *id), 1,
                "{} to {}, {:?}", from, to, steps
            );
        }
        if let Err(e) = check_persist_before_send(&w) {
            return Err(TestCaseError::fail(e));
        }
        for i in [A, B] {
            if let Err(e) = check_restore_is_deterministic(&w, i) {
                return Err(TestCaseError::fail(e));
            }
        }
    }
}

/// The check above is not empty: it passes on a real run, and fails once
/// the sessions' rows are taken out of the log.
#[test]
fn the_send_check_catches_a_message_before_its_session() {
    let steps = [Step::Write(A), Step::Disconnect(B), Step::Write(A)];
    let (mut w, _) = play(&steps);
    assert_eq!(check_persist_before_send(&w), Ok(()));
    for batch in w.log.as_mut().unwrap() {
        batch.effects.retain(
            |e| !matches!(e, Seen::Persist { table, .. } if *table == Table::Sessions as u8),
        );
    }
    let err = check_persist_before_send(&w).unwrap_err();
    assert!(err.contains("before its session was stored"), "{err}");
}
