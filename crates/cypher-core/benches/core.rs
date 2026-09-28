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

use std::time::Instant;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use cypher_core::{Command, MediaKind};
use harness::World;

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

criterion_group!(benches, text, file);
criterion_main!(benches);
