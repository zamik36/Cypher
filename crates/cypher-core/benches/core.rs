//! The sans-IO core end to end, over the scenarios' in-memory world: a text
//! message from command to the peer's stored copy, and a whole file
//! transfer (chunk reads, sealing, acks, the receiver's sink).

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    reason = "shared scenario harness: panics are assertions, fixtures are small"
)]

#[path = "../tests/harness/mod.rs"]
#[expect(dead_code, reason = "benchmarks use a subset of the scenario harness")]
mod harness;

use std::hint::black_box;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use cypher_core::{Command, Core, Effect, Input, MediaKind, StoreOp, Table};
use cypher_crypto::IdentitySeed;
use harness::{Rng, World};
use rand::SeedableRng;

const A: usize = 0;
const B: usize = 1;
const FILE_LEN: usize = 4 * 1024 * 1024;

fn paired() -> World {
    let mut w = World::new(2);
    w.pair(A, B);
    w
}

fn text(c: &mut Criterion) {
    let mut group = c.benchmark_group("core");
    // A fresh pair each time: the first message in a direction pays for a
    // Diffie-Hellman ratchet step on both sides.
    group.bench_function("text_first_message", |b| {
        b.iter_batched_ref(
            paired,
            |w| w.send_text(A, B, "a short chat message"),
            BatchSize::SmallInput,
        );
    });
    // An ongoing conversation: one warmed pair per sample, messages back to
    // back. The peer's delivery receipt flips the ratchet direction every
    // time, so each message still pays for Diffie-Hellman steps.
    group.bench_function("text_steady", |b| {
        b.iter_custom(|iters| {
            let mut w = paired();
            for _ in 0..16 {
                w.send_text(A, B, "warm up");
            }
            let started = Instant::now();
            for _ in 0..iters {
                w.send_text(A, B, "a short chat message");
            }
            started.elapsed()
        });
    });
    group.finish();
}

/// Most keys one message may make the receiver skip.
const MAX_SKIP: usize = 1000;
/// Messages handed to one restored receiver before it is restored again.
const HELD: usize = 32;

/// A paired world where B's session with A keeps `skipped` message keys
/// for messages that never came, and messages from A that B has not been
/// handed yet, all on the chain B reads now.
fn receiver_with_skipped(skipped: usize) -> (World, Vec<Bytes>) {
    let mut w = paired();
    let mut left = skipped;
    while left > 0 {
        let lost = left.min(MAX_SKIP);
        w.lose_messages_from(A, lost);
        for _ in 0..lost {
            w.send_text(A, B, "lost on the way");
        }
        w.send_text(A, B, "arrives");
        left -= lost;
    }
    let mut held = w.hold_for(B, |w| {
        for _ in 0..=HELD {
            w.send_text(A, B, "a short chat message");
        }
    });
    // The first one moves B onto the chain the rest are on.
    w.inject(B, held.remove(0));
    (w, held)
}

/// B as it starts from what it stored, offline.
fn restored_receiver(w: &World) -> Core<Rng> {
    let c = &w.clients[B];
    let (core, _) = Core::restore(
        &IdentitySeed(c.seed),
        c.device,
        &w.snapshot(B),
        w.now,
        Rng::seed_from_u64(9),
    )
    .unwrap();
    core
}

/// Bytes the effects write: keys and sealed values, all of them and those
/// of session rows.
fn written(effects: &[Effect]) -> (usize, usize) {
    effects.iter().fold((0, 0), |(all, sessions), e| match e {
        Effect::Persist(StoreOp::Put { table, key, value }) => {
            let n = key.len() + value.len();
            let s = if *table == Table::Sessions { n } else { 0 };
            (all + n, sessions + s)
        }
        _ => (all, sessions),
    })
}

/// How long B takes to receive `iters` of the `held` messages, restored
/// afresh (untimed) whenever it has taken them all.
fn time_receiving(w: &World, held: &[Bytes], iters: u64) -> Duration {
    let mut spent = Duration::ZERO;
    let mut done = 0;
    while done < iters {
        let mut core = restored_receiver(w);
        for frame in held.iter().take(usize::try_from(iters - done).unwrap()) {
            let frame = Input::Frame(frame.clone());
            let started = Instant::now();
            let effects = core.handle(frame, w.now);
            spent += started.elapsed();
            black_box(effects);
            done += 1;
        }
    }
    spent
}

/// The receiving side alone: B's core takes one message from A that it
/// has not seen, and opens, decrypts, stores and acknowledges it (the
/// receipt waits in the outbox: B is offline). Nothing of A, the server or
/// the storage is timed. `skipped` is how many message keys B's session
/// keeps for messages that never came.
#[expect(
    clippy::print_stderr,
    reason = "the bytes stored are reported beside the time, to compare by hand"
)]
fn receive(c: &mut Criterion) {
    let mut group = c.benchmark_group("core/receive");
    for skipped in [0, 100, 999, 2000] {
        let (w, held) = receiver_with_skipped(skipped);
        let mut core = restored_receiver(&w);
        let (all, sessions) = written(&core.handle(Input::Frame(held[0].clone()), w.now));
        eprintln!(
            "core/receive/{skipped}: {all} B written per message, {sessions} B of it session rows"
        );
        group.bench_with_input(BenchmarkId::from_parameter(skipped), &held, |b, held| {
            b.iter_custom(|iters| time_receiving(&w, held, iters));
        });
    }
    group.finish();
}

/// A paired world with a file offered by A and not yet accepted by B.
fn offered() -> (World, cypher_types::FileId) {
    let mut w = paired();
    let (file_id, msg_id, peer) = (w.file_id(), w.msg_id(), w.peer(B));
    w.clients[A].sources.insert(file_id, vec![0x5A; FILE_LEN]);
    w.command(
        A,
        Command::SendFile {
            peer,
            msg_id,
            file_id,
            name: "big.bin".into(),
            mime: "application/octet-stream".into(),
            size: FILE_LEN as u64,
            kind: MediaKind::File,
            inline: None,
        },
    );
    (w, file_id)
}

fn file(c: &mut Criterion) {
    let mut group = c.benchmark_group("core");
    group.throughput(Throughput::Bytes(FILE_LEN as u64));
    group.sample_size(20);
    group.bench_function("file_transfer_4m", |b| {
        b.iter_batched_ref(
            offered,
            |(w, file_id)| {
                w.command(B, Command::AcceptFile { file_id: *file_id });
                assert_eq!(w.clients[B].sinks[file_id].len(), FILE_LEN);
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

criterion_group!(benches, text, receive, file);
criterion_main!(benches);
