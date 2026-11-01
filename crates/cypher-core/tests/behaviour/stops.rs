//! The app stops after any row a batch writes, the rest lost, for every
//! batch that touches the rows kept beside a session: a key set aside, a
//! key taken, a session replaced, a session read from a release that kept
//! its keys inside, keys expired, a session deleted. Whatever was written,
//! a restart finds each session as the last of its rows that was written
//! left it, and so does a restart that stops again partway through the
//! rows the restore itself writes.

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;
use cypher_core::{Command, Core, Effect, Input, StoreOp, Table, Vault};
use cypher_crypto::IdentitySeed;
use cypher_types::DeviceId;
use rand::SeedableRng;

use super::addr;
use super::replays::{altered, init_in, last_message_from, over_session};
use super::rows::{DecryptedRow, Session, SkippedKey};
use super::skipped::{FIXTURE, Fixture, KEPT_MS, held, rows_of, snapshot, write};
use crate::harness::{Rng, World};
use crate::{A, B, paired};

type Rows = Vec<(u8, Vec<u8>, Vec<u8>)>;

/// How long a session's own key is: the rows beside it extend it.
const SESSION_KEY_LEN: usize = 36;

/// A device's store, and how it starts from it.
struct Store {
    seed: [u8; 32],
    device: DeviceId,
    now: u64,
}

/// A session's row: ratchet, pending init, inits accepted, `Hello` sent.
type SessionRow = (Vec<u8>, Vec<u8>, Vec<[u8; 32]>, bool);
/// A skipped key's row: ratchet key, number, message key, date.
type KeyRow = ([u8; 32], u32, [u8; 32], u64);

/// What a session's rows hold, as a restart leaves them: its row (without
/// the keys it names to delete, and without its lineage, made afresh when
/// a session is read from an older release), its keys and its digests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct State {
    session: Option<SessionRow>,
    keys: BTreeMap<u64, KeyRow>,
    digests: BTreeSet<(u64, [u8; 32])>,
}

impl Store {
    fn of(w: &World, i: usize) -> Self {
        Self {
            seed: w.clients[i].seed,
            device: w.clients[i].device,
            now: w.now,
        }
    }

    fn restore(&self, rows: &Rows) -> (Core<Rng>, Vec<Effect>) {
        Core::restore(
            &IdentitySeed(self.seed),
            self.device,
            &snapshot(rows),
            self.now,
            Rng::seed_from_u64(3),
        )
        .unwrap()
    }

    /// Each session's state once the device restarted from `rows` and
    /// wrote all the restore writes.
    fn states(&self, rows: &Rows) -> BTreeMap<Vec<u8>, State> {
        let mut rows = rows.clone();
        let (_, effects) = self.restore(&rows);
        for op in writes(effects) {
            write(&mut rows, op);
        }
        self.read(&rows)
    }

    /// Each session's state as `rows` hold it. Every row kept beside a
    /// session has the session's row, and its lineage.
    fn read(&self, rows: &Rows) -> BTreeMap<Vec<u8>, State> {
        let vault = Vault::new(IdentitySeed(self.seed).derive_storage_key());
        let mut out: BTreeMap<Vec<u8>, State> = BTreeMap::new();
        let mut lineages = BTreeMap::new();
        let sessions = rows.iter().filter(|(t, ..)| *t == Table::Sessions as u8);
        for (_, k, v) in sessions
            .clone()
            .filter(|(_, k, _)| k.len() == SESSION_KEY_LEN)
        {
            let s: Session = vault.open(Table::Sessions, k, v).unwrap();
            lineages.insert(k.clone(), s.lineage);
            out.entry(k.clone()).or_default().session = Some((
                s.ratchet,
                s.pending_init,
                s.accepted_ephemerals,
                s.hello_sent,
            ));
        }
        for (_, k, v) in sessions.filter(|(_, k, _)| k.len() > SESSION_KEY_LEN) {
            let (session, rest) = k.split_at(SESSION_KEY_LEN);
            let lineage = lineages.get(session);
            assert!(lineage.is_some(), "a row beside a session it outlived");
            let state = out.entry(session.to_vec()).or_default();
            match rest.split_first() {
                Some((1, seq)) => {
                    let key: SkippedKey = vault.open(Table::Sessions, k, v).unwrap();
                    assert_eq!(Some(&key.lineage), lineage, "a key of another session");
                    let seq = u64::from_be_bytes(seq.try_into().unwrap());
                    state
                        .keys
                        .insert(seq, (key.dh, key.n, key.mk, key.created_ms));
                }
                Some((2, _)) => {
                    let d: DecryptedRow = vault.open(Table::Sessions, k, v).unwrap();
                    assert_eq!(Some(&d.lineage), lineage, "a digest of another session");
                    state.digests.insert((d.seq, d.digest));
                }
                _ => panic!("a row no release writes: {k:?}"),
            }
        }
        out
    }

    /// Stops after each of the rows `ops` writes from `before`, and again
    /// after each row of the restore that follows, and checks every
    /// session against the last of its rows that was written.
    fn stop_anywhere(&self, what: &str, before: &Rows, ops: &[StoreOp]) {
        let all = self.states(&with(before, ops));
        for stop in 0..=ops.len() {
            let rows = with(before, &ops[..stop]);
            let got = self.states(&rows);
            for session in got.keys().chain(all.keys()) {
                let last = last_row_of(session, &ops[..stop]);
                let expected = self.states(&with(before, &ops[..last]));
                let what = format!("{what}, stopped after {stop} rows");
                same(&what, session, &expected, &got, &all);
                if last > 0 && last == last_row_of(session, ops) {
                    let [expected, all] = [&expected, &all].map(|m| m.get(session).cloned());
                    assert_eq!(
                        expected.map(|s| (s.session, s.keys)),
                        all.map(|s| (s.session, s.keys)),
                        "{what}: its last row stored is not all it holds"
                    );
                }
            }
            let (_, effects) = self.restore(&rows);
            let restore = writes(effects);
            for again in 0..=restore.len() {
                let twice = self.states(&with(&rows, &restore[..again]));
                assert_eq!(
                    twice, got,
                    "{what}: stopped after {stop} rows, then after {again} the restore wrote"
                );
            }
        }
    }
}

/// `before` with `ops` written.
fn with(before: &Rows, ops: &[StoreOp]) -> Rows {
    let mut rows = before.clone();
    for op in ops {
        write(&mut rows, op.clone());
    }
    rows
}

/// How many of `ops` there are up to the last that wrote or deleted the
/// row of `session` itself.
fn last_row_of(session: &[u8], ops: &[StoreOp]) -> usize {
    ops.iter()
        .rposition(|op| match op {
            StoreOp::Put { table, key, .. } | StoreOp::Delete { table, key } => {
                *table == Table::Sessions && key == session
            }
        })
        .map_or(0, |i| i + 1)
}

/// The session is as its last written row left it: its row and keys are
/// those, and its digests are those and at most the ones its batch wrote
/// after that row.
fn same(
    what: &str,
    session: &[u8],
    expected: &BTreeMap<Vec<u8>, State>,
    got: &BTreeMap<Vec<u8>, State>,
    all: &BTreeMap<Vec<u8>, State>,
) {
    let [expected, got, all] =
        [expected, got, all].map(|m| m.get(session).cloned().unwrap_or_default());
    assert_eq!(
        (&got.session, &got.keys),
        (&expected.session, &expected.keys),
        "{what}"
    );
    let added: BTreeSet<_> = expected.digests.union(&all.digests).copied().collect();
    assert!(
        expected.digests.is_subset(&got.digests) && got.digests.is_subset(&added),
        "{what}: digests {:?}",
        got.digests
    );
}

fn writes(effects: Vec<Effect>) -> Vec<StoreOp> {
    effects
        .into_iter()
        .filter_map(|e| match e {
            Effect::Persist(op) => Some(op),
            _ => None,
        })
        .collect()
}

/// B, which keeps keys for two messages from A that never came.
fn skipping() -> World {
    let mut w = paired();
    held(&mut w, &["late 1", "late 2"]);
    w.send_text(A, B, "on time");
    w
}

/// What B writes taking `input` from the rows it stored, offline.
fn batch(w: &World, input: Input) -> (Store, Rows, Vec<StoreOp>) {
    let device = Store::of(w, B);
    let before = rows_of(w, B);
    let (mut core, _) = device.restore(&before);
    let ops = writes(core.handle(input, device.now));
    (device, before, ops)
}

/// A message that sets a key aside, and one that takes one.
#[test]
fn a_stop_while_keys_are_set_aside_or_taken_leaves_each_session_whole() {
    let mut w = paired();
    let late = held(&mut w, &["late"]);
    w.send_text(A, B, "on time");
    let sets_aside = held(&mut w, &["late too", "sets one aside"]);
    for (what, frame) in [
        ("a key set aside", sets_aside[1].clone()),
        ("a key taken", late[0].clone()),
    ] {
        let (device, before, ops) = batch(&w, Input::Frame(frame));
        device.stop_anywhere(what, &before, &ops);
    }
}

/// A fresh session from A replaces B's, whose keys go with it.
#[test]
fn a_stop_while_a_session_is_replaced_leaves_it_whole() {
    let mut w = skipping();
    w.send_text(B, A, "reply");
    let b = addr(&w, B);
    let last = last_message_from(&w, B);
    let frames = w.hold_for(B, |w| w.inject(A, over_session(b, altered(&last))));
    let init: Bytes = frames
        .into_iter()
        .find(|f| init_in(&w, B, f).is_some())
        .expect("A starts a fresh session with B");
    let (device, before, ops) = batch(&w, Input::Frame(init));
    assert!(
        ops.iter()
            .any(|op| matches!(op, StoreOp::Delete { key, .. } if key.len() > SESSION_KEY_LEN)),
        "the old session's keys go"
    );
    device.stop_anywhere("a session replaced", &before, &ops);
}

/// A tick finds B's keys kept 30 days.
#[test]
fn a_stop_while_keys_expire_leaves_each_session_whole() {
    let mut w = skipping();
    w.now += KEPT_MS;
    let (device, before, ops) = batch(&w, Input::Tick);
    assert!(!ops.is_empty(), "the keys expire");
    device.stop_anywhere("keys expired", &before, &ops);
}

/// B deletes its contact A, with the session and the rows beside it.
#[test]
fn a_stop_while_a_session_is_deleted_leaves_no_row_beside_it() {
    let w = skipping();
    let peer = w.peer(A);
    let (device, before, ops) = batch(&w, Input::Command(Command::RemovePeer { peer }));
    let session = cypher_core::session_key(addr(&w, A));
    let left = with(&before, &ops)
        .into_iter()
        .filter(|(t, k, _)| *t == Table::Sessions as u8 && k.starts_with(&session))
        .count();
    assert_eq!(left, 0, "the batch itself deletes every row of the session");
    device.stop_anywhere("a session deleted", &before, &ops);
}

/// B, updated from a release that kept a session's keys inside it, stores
/// the session again the way this release does.
#[test]
fn a_stop_while_a_session_is_stored_anew_after_an_update_leaves_it_whole() {
    let fixture: Fixture = postcard::from_bytes(FIXTURE).unwrap();
    let device = Store {
        seed: fixture.seed,
        device: DeviceId(fixture.device),
        now: fixture.now,
    };
    let (_, effects) = device.restore(&fixture.rows);
    let ops = writes(effects);
    assert!(ops.len() > 3, "the session and its three keys");
    device.stop_anywhere("a session stored anew", &fixture.rows, &ops);
}
