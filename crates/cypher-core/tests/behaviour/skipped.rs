//! Messages that arrive late, after later ones from the same device: the
//! keys kept for them read them once, across restarts and ratchet steps,
//! and a session stored before those keys were kept apart still does.

use bytes::Bytes;
use cypher_core::{Content, Core, Event, FailReason, Input, Snapshot, StoreOp, Table};
use cypher_crypto::IdentitySeed;
use cypher_types::DeviceId;
use rand::SeedableRng;
use serde::Deserialize;

use super::events_since;
use crate::harness::{Rng, World};
use crate::{A, B, paired};

/// Whether client `i` was told, since event `from`, that a message did not
/// decrypt.
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

/// A sends `texts` to B, which the server holds back from B.
fn held(w: &mut World, texts: &[&str]) -> Vec<Bytes> {
    w.hold_for(B, |w| {
        for text in texts {
            w.send_text(A, B, text);
        }
    })
}

/// A message held back reaches B after a later one did and after B
/// restarted: B reads it, and nothing warns.
#[test]
fn a_late_message_is_read_after_a_restart() {
    let mut w = paired();
    let late = held(&mut w, &["late"]);
    w.send_text(A, B, "on time");
    w.restart(B);
    let from = w.clients[B].events.len();

    w.inject(B, late[0].clone());

    assert_eq!(w.texts(B), ["on time", "late"]);
    assert!(!warned(&w, B, from));
}

/// Messages held back on a chain A left behind since, and on the one it
/// writes on now, reach B after B restarted: B reads every one, in
/// whatever order they come.
#[test]
fn late_messages_from_chains_left_behind_are_read_after_a_restart() {
    let mut w = paired();
    let mut late = held(&mut w, &["late 1", "late 2"]);
    w.send_text(A, B, "on time 1");
    w.send_text(B, A, "reply");
    late.extend(held(&mut w, &["late 3"]));
    w.send_text(A, B, "on time 2");
    w.restart(B);
    let from = w.clients[B].events.len();

    for i in [2, 0, 1] {
        w.inject(B, late[i].clone());
    }

    assert_eq!(
        w.texts(B),
        ["on time 1", "on time 2", "late 3", "late 1", "late 2"]
    );
    assert!(!warned(&w, B, from));
}

/// What `tests/fixtures/session_v2.bin` holds: B's stored rows from a
/// release that kept a session's skipped message keys inside its ratchet
/// (session record version 2), the three messages from A that B had
/// skipped, and their texts. It was written by that release from the world
/// of [`late_messages_from_chains_left_behind_are_read_after_a_restart`],
/// just before B restarts.
#[derive(Deserialize)]
struct Fixture {
    seed: [u8; 32],
    device: u32,
    now: u64,
    /// `(table, key, sealed value)`.
    rows: Vec<(u8, Vec<u8>, Vec<u8>)>,
    late: Vec<Vec<u8>>,
    texts: Vec<String>,
}

const FIXTURE: &[u8] = include_bytes!("../fixtures/session_v2.bin");

fn snapshot(rows: &[(u8, Vec<u8>, Vec<u8>)]) -> Snapshot {
    let pick = |t: Table| {
        rows.iter()
            .filter(|(table, ..)| *table == t as u8)
            .map(|(_, k, v)| (k.clone(), v.clone()))
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

/// Applies the rows `effects` write or delete to `rows`; the events.
fn apply(rows: &mut Vec<(u8, Vec<u8>, Vec<u8>)>, effects: Vec<cypher_core::Effect>) -> Vec<Event> {
    let mut events = Vec::new();
    for effect in effects {
        match effect {
            cypher_core::Effect::Persist(StoreOp::Put { table, key, value }) => {
                rows.retain(|(t, k, _)| (*t, k) != (table as u8, &key));
                rows.push((table as u8, key, value));
            }
            cypher_core::Effect::Persist(StoreOp::Delete { table, key }) => {
                rows.retain(|(t, k, _)| (*t, k) != (table as u8, &key));
            }
            cypher_core::Effect::Emit(event) => events.push(event),
            _ => {}
        }
    }
    events
}

/// B's session was stored by a release that kept its skipped message keys
/// inside the ratchet. Restored, stored again the way this release stores
/// it and restored once more, it still reads the three messages it
/// skipped, and nothing is taken for damaged.
#[test]
fn a_session_stored_with_its_skipped_keys_inside_still_reads_late_messages() {
    let fixture: Fixture = postcard::from_bytes(FIXTURE).unwrap();
    let seed = IdentitySeed(fixture.seed);
    let device = DeviceId(fixture.device);
    let mut rows = fixture.rows;
    let mut events = Vec::new();

    let restore = |rows: &[(u8, Vec<u8>, Vec<u8>)], seed_rng: u64| {
        Core::restore(
            &seed,
            device,
            &snapshot(rows),
            fixture.now,
            Rng::seed_from_u64(seed_rng),
        )
        .unwrap()
    };
    let (_, effects) = restore(&rows, 0);
    events.extend(apply(&mut rows, effects));
    let (mut core, effects) = restore(&rows, 1);
    events.extend(apply(&mut rows, effects));
    for frame in fixture.late {
        let effects = core.handle(Input::Frame(Bytes::from(frame)), fixture.now);
        events.extend(apply(&mut rows, effects));
    }

    let texts: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::Message(m) => match &m.content {
                Content::Text { text, .. } => Some(text.as_str()),
                Content::File { .. } => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(texts, fixture.texts);
    assert!(
        !events.iter().any(|e| matches!(e, Event::Warning { .. })),
        "{events:?}"
    );
}
