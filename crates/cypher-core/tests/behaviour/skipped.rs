//! Messages that arrive late, after later ones from the same device: the
//! keys kept for them read them once, across restarts and ratchet steps,
//! and a session stored before those keys were kept apart still does.

use bytes::Bytes;
use cypher_core::{
    Command, Content, Core, Effect, Event, FailReason, Input, Record, Snapshot, StoreOp, Table,
    Vault, session_key,
};
use cypher_crypto::IdentitySeed;
use cypher_types::{Addr, DeviceId};
use rand::SeedableRng;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

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
    reads_the_late_messages(&fixture, fixture.rows.clone());
}

/// Releases before session record version 2 kept the same ratchet
/// snapshot, skipped keys inside, in a session record of version 1, and
/// before that inside the contact's own record (version 4). B's rows from
/// the fixture, written back in those shapes, read the three messages too.
#[test]
fn older_session_and_contact_records_with_keys_inside_read_late_messages() {
    let fixture: Fixture = postcard::from_bytes(FIXTURE).unwrap();
    let vault = Vault::new(IdentitySeed(fixture.seed).derive_storage_key());
    let is_session = |(t, k, _): &&(u8, Vec<u8>, Vec<u8>)| {
        *t == Table::Sessions as u8 && k.len() == session_key_len()
    };
    let (_, session_key, sealed) = fixture.rows.iter().find(is_session).unwrap().clone();
    let session: SessionV2 = vault.open(Table::Sessions, &session_key, &sealed).unwrap();
    let is_peer = |(t, _, _): &&(u8, Vec<u8>, Vec<u8>)| *t == Table::Peers as u8;
    let (_, peer_key, sealed) = fixture.rows.iter().find(is_peer).unwrap().clone();
    let contact: ContactV7 = vault.open(Table::Peers, &peer_key, &sealed).unwrap();

    let v1 = SessionV1 {
        ratchet: session.ratchet.clone(),
        pending_init: session.pending_init.clone(),
        accepted_ephemerals: session.accepted_ephemerals.clone(),
        inbox: None,
        hello_sent: session.hello_sent,
    };
    let mut rows = fixture.rows.clone();
    write(
        &mut rows,
        vault.put(Table::Sessions, session_key.clone(), &v1, &mut OsRng),
    );
    reads_the_late_messages(&fixture, rows);

    let v4 = ContactV4 {
        ratchet: session.ratchet,
        pending_init: session.pending_init,
        accepted_ephemerals: session.accepted_ephemerals,
        identity_dh: contact.identity_dh,
        inbox: None,
        hello_sent: session.hello_sent,
        alias: contact.alias,
        name: contact.name,
        request: contact.request,
        blocked: contact.blocked,
        via: contact.via,
    };
    let mut rows = fixture.rows.clone();
    write(
        &mut rows,
        StoreOp::Delete {
            table: Table::Sessions,
            key: session_key,
        },
    );
    write(
        &mut rows,
        vault.put(Table::Peers, peer_key, &v4, &mut OsRng),
    );
    reads_the_late_messages(&fixture, rows);
}

fn session_key_len() -> usize {
    session_key(Addr::new(cypher_types::PeerId([0; 32]), DeviceId::FIRST)).len()
}

/// B, from `rows`, restored, stored again the way this release stores it
/// and restored once more, reads the fixture's late messages, and nothing
/// warns.
fn reads_the_late_messages(fixture: &Fixture, mut rows: Vec<(u8, Vec<u8>, Vec<u8>)>) {
    let seed = IdentitySeed(fixture.seed);
    let device = DeviceId(fixture.device);
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
    for frame in &fixture.late {
        let effects = core.handle(Input::Frame(Bytes::from(frame.clone())), fixture.now);
        events.extend(apply(&mut rows, effects));
    }

    assert_eq!(texts_of(&events), fixture.texts);
    assert!(
        !events.iter().any(|e| matches!(e, Event::Warning { .. })),
        "{events:?}"
    );
}

/// An inbox announcement as the records keep it.
type Ad = (u32, [[u8; 32]; 2], [[u8; 32]; 2]);

/// A session as record version 2 stored it, the one the fixture holds.
#[derive(Serialize, Deserialize)]
struct SessionV2 {
    ratchet: Vec<u8>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    inbox: Option<Ad>,
    hello_sent: bool,
}

impl Record for SessionV2 {
    const VERSION: u8 = 2;
}

/// A session as record version 1 stored it.
#[derive(Serialize, Deserialize)]
struct SessionV1 {
    ratchet: Vec<u8>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
}

impl Record for SessionV1 {
    const VERSION: u8 = 1;
}

/// A contact as version 7 stores it, the one the fixture holds.
#[derive(Serialize, Deserialize)]
struct ContactV7 {
    identity_dh: [u8; 32],
    alias: Option<String>,
    name: Option<String>,
    request: bool,
    blocked: bool,
    via: Option<String>,
    devices: Option<Vec<u8>>,
    inboxes: Vec<(u32, Ad)>,
}

impl Record for ContactV7 {
    const VERSION: u8 = 7;
}

/// A contact and its one session as version 4 stored them.
#[derive(Serialize, Deserialize)]
struct ContactV4 {
    ratchet: Vec<u8>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    identity_dh: [u8; 32],
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
    alias: Option<String>,
    name: Option<String>,
    request: bool,
    blocked: bool,
    via: Option<String>,
}

impl Record for ContactV4 {
    const VERSION: u8 = 4;
}

/// Client `i`'s stored rows, as `(table, key, sealed value)`.
fn rows_of(w: &World, i: usize) -> Vec<(u8, Vec<u8>, Vec<u8>)> {
    w.clients[i]
        .kv
        .iter()
        .map(|((t, k), v)| (*t, k.clone(), v.clone()))
        .collect()
}

/// What follows the session's key in the key of a row of each kind kept
/// beside it.
const SKIPPED_KEY: u8 = 1;
const DIGEST: u8 = 2;

/// The rows of kind `kind` kept beside the session with `with` among
/// `rows`.
fn beside(rows: &[(u8, Vec<u8>, Vec<u8>)], with: Addr, kind: u8) -> usize {
    let session = session_key(with);
    rows.iter()
        .filter(|(t, k, _)| {
            *t == Table::Sessions as u8
                && k.starts_with(&session)
                && k.get(session.len()) == Some(&kind)
        })
        .count()
}

/// The skipped keys stored beside the session with `with` among `rows`.
fn rows_beside(rows: &[(u8, Vec<u8>, Vec<u8>)], with: Addr) -> usize {
    beside(rows, with, SKIPPED_KEY)
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
/// with them, a later message stores that row again and its own digest,
/// no key, a late message read deletes its key, and removing the contact
/// deletes them all, digests too.
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
    let mut digests = 0;
    for batch in w.log.take().unwrap().iter().filter(|b| b.client == B) {
        for seen in &batch.effects {
            if let crate::harness::Seen::Persist { table, key } = seen
                && *table == Table::Sessions as u8
                && *key != session
            {
                assert_eq!(key.get(session.len()), Some(&DIGEST), "no key stored");
                digests += 1;
            }
        }
    }
    assert_eq!(digests, 1, "the one message read");

    w.inject(B, late[1].clone());
    assert_eq!(rows_beside(&rows_of(&w, B), a), 2);
    let peer = w.peer(A);
    assert!(beside(&rows_of(&w, B), a, DIGEST) > 0);
    w.command(B, Command::RemovePeer { peer });
    assert_eq!(rows_beside(&rows_of(&w, B), a), 0);
    assert_eq!(beside(&rows_of(&w, B), a, DIGEST), 0);
    assert!(rows::session(&w, B, a).is_none());
}

/// How long a key set aside for a late message is kept: 30 days.
const KEPT_MS: u64 = 30 * 24 * 3600 * 1000;

/// A key kept for a late message is kept 30 days and deleted once they are
/// over: the message, coming later still, is not read, and warns.
#[test]
fn a_skipped_key_is_deleted_after_thirty_days_and_opens_nothing() {
    let mut w = paired();
    let a = addr(&w, A);
    let late = held(&mut w, &["late"]);
    w.send_text(A, B, "on time");
    w.now += KEPT_MS - 1_000;
    w.advance(500);
    assert_eq!(rows_beside(&rows_of(&w, B), a), 1, "kept until then");

    w.advance(1_000);
    assert_eq!(rows_beside(&rows_of(&w, B), a), 0, "deleted once over");
    let from = w.clients[B].events.len();
    w.inject(B, late[0].clone());
    assert!(warned(&w, B, from));
    assert_eq!(w.texts(B), ["on time"]);
}

/// A key whose 30 days ran out while the app was closed is deleted soon
/// after it starts again.
#[test]
fn a_skipped_key_that_ran_out_meanwhile_is_deleted_after_a_restart() {
    let mut w = paired();
    let a = addr(&w, A);
    held(&mut w, &["late"]);
    w.send_text(A, B, "on time");
    w.now += KEPT_MS;
    w.restart(B);
    w.advance(500);
    assert_eq!(rows_beside(&rows_of(&w, B), a), 0);
}

/// B's clock ran ten days ahead when it set a key aside, and was set right
/// later: the key is deleted 30 days after the clock was set right, not
/// ten days past that, whether or not B restarted meanwhile.
#[test]
fn a_skipped_key_dated_ahead_of_the_clock_is_kept_thirty_days_by_it() {
    const DAY_MS: u64 = 24 * 3600 * 1000;
    let mut w = paired();
    let a = addr(&w, A);
    held(&mut w, &["late"]);
    w.clients[B].ahead = 10 * DAY_MS;
    w.send_text(A, B, "on time");
    w.clients[B].ahead = 0;
    w.advance(500);
    w.restart(B);
    w.now += KEPT_MS - 1_000;
    w.advance(500);
    assert_eq!(rows_beside(&rows_of(&w, B), a), 1, "kept until then");

    w.advance(1_000);
    assert_eq!(rows_beside(&rows_of(&w, B), a), 0, "deleted once over");
}

/// B takes a message that makes it set a key aside, or one that takes a
/// key set aside, and the app stops after any of the rows that message
/// writes (the rest are lost). Restored from what was written, B neither
/// uses a key twice nor loses one: every message still to come is read
/// once, the message itself is read again if and only if its session row
/// was not stored (and is no news, without a warning, once its digest
/// was), and once every late message is read no key is left beside the
/// session.
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
    let digest = written.iter().any(|op| {
        matches!(op, StoreOp::Put { table: Table::Sessions, key, .. }
            if key.starts_with(&session) && key.get(session.len()) == Some(&DIGEST))
    });

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
        if digest {
            assert!(
                again.is_empty(),
                "step {step}: a copy is no news: {again:?}"
            );
        }
    }
}
