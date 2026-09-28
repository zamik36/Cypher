//! Frames on the gateway's hot path: every relayed message is peeked and
//! re-encoded, clients encode and decode full frames.

#![expect(clippy::unwrap_used, reason = "benchmark fixtures fail loudly")]

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use cypher_types::PeerId;
use cypher_wire::{ClientMsg, Frame, encode_recv, peek_send};

fn send_frame(len: usize) -> Bytes {
    Frame::new(
        7,
        ClientMsg::Send {
            to: PeerId([9; 32]),
            want_ack: true,
            body: Bytes::from(vec![0xA5; len]),
        },
    )
    .encode()
}

fn frames(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire");
    for (name, len) in [("1k", 1024), ("256k", 256 * 1024)] {
        group.throughput(Throughput::Bytes(len as u64));
        let frame = send_frame(len);
        let body = vec![0xA5u8; len];
        group.bench_function(format!("encode_send_{name}"), |b| {
            b.iter(|| send_frame(len));
        });
        group.bench_function(format!("decode_send_{name}"), |b| {
            b.iter(|| Frame::<ClientMsg>::decode(frame.clone()).unwrap());
        });
        group.bench_function(format!("peek_send_{name}"), |b| {
            b.iter(|| peek_send(&frame).unwrap());
        });
        group.bench_function(format!("encode_recv_{name}"), |b| {
            b.iter(|| encode_recv(&PeerId([3; 32]), &body));
        });
    }
    group.finish();
}

criterion_group!(benches, frames);
criterion_main!(benches);
