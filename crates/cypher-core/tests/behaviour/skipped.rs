//! Messages that arrive late, after later ones from the same device: the
//! keys kept for them read them once, across restarts and ratchet steps,
//! and a session stored before those keys were kept apart still does.

use bytes::Bytes;
use cypher_core::{
    Command, Content, Core, Effect, Event, FailReason, Input, Snapshot, StoreOp, Table, session_key,
};
use cypher_crypto::IdentitySeed;
use cypher_types::{Addr, DeviceId};
use rand::SeedableRng;
use serde::Deserialize;

use super::{addr, events_since, rows};
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
fn apply(rows: &mut Vec<(u8, Vec<u8>, Vec<u8>)>, effects: Vec<Effect>) -> Vec<Event> {
    let mut events = Vec::new();
    for effect in effects {
        match effect {
            Effect::Persist(op) => write(rows, op),
            Effect::Emit(event) => events.push(event),
            _ => {}
        }
    }
    events
}

fn write(rows: &mut Vec<(u8, Vec<u8>, Vec<u8>)>, op: StoreOp) {
    match op {
        StoreOp::Put { table, key, value } => {
            rows.retain(|(t, k, _)| (*t, k) != (table as u8, &key));
            rows.push((table as u8, key, value));
        }
        StoreOp::Delete { table, key } => {
            rows.retain(|(t, k, _)| (*t, k) != (table as u8, &key));
        }
    }
}

/// The texts of the messages `events` show.
fn texts_of(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Message(m) => match &m.content {
                Content::Text { text, .. } => Some(text.as_str()),
                Content::File { .. } => None,
            },
            _ => None,
        })
        .collect()
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

    assert_eq!(texts_of(&events), fixture.texts);
    assert!(
        !events.iter().any(|e| matches!(e, Event::Warning { .. })),
        "{events:?}"
    );
}

/// Client `i`'s stored rows, as `(table, key, sealed value)`.
fn rows_of(w: &World, i: usize) -> Vec<(u8, Vec<u8>, Vec<u8>)> {
    w.clients[i]
        .kv
        .iter()
        .map(|((t, k), v)| (*t, k.clone(), v.clone()))
        .collect()
}

/// The rows kept beside the session with `with` among `rows`.
fn rows_beside(rows: &[(u8, Vec<u8>, Vec<u8>)], with: Addr) -> usize {
    let session = session_key(with);
    rows.iter()
        .filter(|(t, k, _)| {
            *t == Table::Sessions as u8 && k.len() > session.len() && k.starts_with(&session)
        })
        .count()
}

/// Client `i` as it starts from `rows`, offline.
fn restored(
    w: &World,
    i: usize,
    rows: &[(u8, Vec<u8>, Vec<u8>)],
    rng: u64,
) -> (Core<Rng>, Vec<Effect>) {
    let c = &w.clients[i];
    Core::restore(
        &IdentitySeed(c.seed),
        c.device,
        &snapshot(rows),
        w.now,
        Rng::seed_from_u64(rng),
    )
    .unwrap()
}

/// The keys B sets aside for messages it skipped are stored beside its
/// session, one row each, not inside it: the session's row does not grow
/// with them, a later message stores only that row again, a late message
/// read deletes its key, and removing the contact deletes them all.
#[test]
fn skipped_keys_are_stored_beside_their_session_and_go_with_it() {
    let mut w = paired();
    let a = addr(&w, A);
    let empty = rows::session(&w, B, a).unwrap().ratchet.len();
    let late = held(&mut w, &["late 1", "late 2", "late 3"]);
    w.send_text(A, B, "on time");
    assert_eq!(rows_beside(&rows_of(&w, B), a), 3);
    assert!(rows::session(&w, B, a).unwrap().ratchet.len() <= empty + 1);

    w.log = Some(Vec::new());
    w.send_text(A, B, "next");
    let session = session_key(a);
    for batch in w.log.take().unwrap().iter().filter(|b| b.client == B) {
        for seen in &batch.effects {
            if let crate::harness::Seen::Persist { table, key } = seen
                && *table == Table::Sessions as u8
            {
                assert_eq!(*key, session, "only the session's own row");
            }
        }
    }

    w.inject(B, late[1].clone());
    assert_eq!(rows_beside(&rows_of(&w, B), a), 2);
    let peer = w.peer(A);
    w.command(B, Command::RemovePeer { peer });
    assert_eq!(rows_beside(&rows_of(&w, B), a), 0);
    assert!(rows::session(&w, B, a).is_none());
}

/// B takes a message that makes it set a key aside, or one that takes a
/// key set aside, and the app stops after any of the rows that message
/// writes (the rest are lost). Restored from what was written, B neither
/// uses a key twice nor loses one: every message still to come is read
/// once, the message itself is read again if and only if its session row
/// was not stored, and once every late message is read no row is left
/// beside the session.
#[test]
fn a_stop_after_any_row_written_neither_reuses_nor_loses_a_skipped_key() {
    let mut w = paired();
    let late = held(&mut w, &["late 1", "late 2"]);
    w.send_text(A, B, "on time");
    let ahead = held(&mut w, &["late 3", "sets a key aside"]);
    let before = rows_of(&w, B);
    let frames = [
        (late[0].clone(), "late 1"),
        (late[1].clone(), "late 2"),
        (ahead[0].clone(), "late 3"),
        (ahead[1].clone(), "sets a key aside"),
    ];

    for step in [3, 0] {
        let (mut core, _) = restored(&w, B, &before, 0);
        let effects = core.handle(Input::Frame(frames[step].0.clone()), w.now);
        let ops: Vec<StoreOp> = effects
            .into_iter()
            .filter_map(|e| match e {
                Effect::Persist(op) => Some(op),
                _ => None,
            })
            .collect();
        assert!(ops.len() > 2, "the message writes several rows");
        for stop in 0..=ops.len() {
            restart_after(&w, &before, &frames, step, &ops[..stop]);
        }
    }
}

/// B, stopped once `written` (the first rows message `step` wrote) were
/// stored, starts again from `before` and them and is handed every frame
/// once more; see the test above for what it must make of them.
fn restart_after(
    w: &World,
    before: &[(u8, Vec<u8>, Vec<u8>)],
    frames: &[(Bytes, &str)],
    step: usize,
    written: &[StoreOp],
) {
    let a = addr(w, A);
    let session = session_key(a);
    let stop = written.len();
    let mut rows = before.to_vec();
    for op in written {
        write(&mut rows, op.clone());
    }
    let committed = written.iter().any(
        |op| matches!(op, StoreOp::Put { table: Table::Sessions, key, .. } if *key == session),
    );

    let (mut core, effects) = restored(w, B, &rows, 1);
    let mut events = apply(&mut rows, effects);
    let mut expected: Vec<&str> = Vec::new();
    for (i, (frame, text)) in frames.iter().enumerate() {
        if i == step && committed {
            continue;
        }
        expected.push(text);
        let effects = core.handle(Input::Frame(frame.clone()), w.now);
        events.extend(apply(&mut rows, effects));
    }
    let mut read = texts_of(&events);
    read.sort_unstable();
    expected.sort_unstable();
    assert_eq!(read, expected, "step {step}, stopped after {stop} rows");
    assert!(
        !events.iter().any(|e| matches!(e, Event::Warning { .. })),
        "step {step}, stopped after {stop} rows: {events:?}"
    );
    assert_eq!(
        rows_beside(&rows, a),
        0,
        "step {step}, stopped after {stop} rows"
    );

    if committed {
        let again = core.handle(Input::Frame(frames[step].0.clone()), w.now);
        let again = apply(&mut rows, again);
        assert!(texts_of(&again).is_empty(), "step {step}: read twice");
    }
}
