//! Voice notes: Opus encoding speed against real time (with and without
//! resampling) and `WebM` muxing.

#![expect(clippy::unwrap_used, reason = "benchmark fixtures fail loudly")]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use cypher_media::VoiceEncoder;
use cypher_media::webm::OpusWebm;

const SECONDS: u32 = 10;

/// A speech-like signal: two tones with a slow amplitude envelope.
fn signal(rate: u32) -> Vec<f32> {
    (0..rate * SECONDS)
        .map(|i| {
            let t = f64::from(i) / f64::from(rate);
            let envelope = 0.5 + 0.5 * (t * 3.0).sin();
            let tone = (t * 220.0 * std::f64::consts::TAU).sin()
                + 0.5 * (t * 530.0 * std::f64::consts::TAU).sin();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the signal stays within [-1.5, 1.5]"
            )]
            let sample = (0.4 * envelope * tone) as f32;
            sample
        })
        .collect()
}

fn voice(c: &mut Criterion) {
    let mut group = c.benchmark_group("voice");
    group.sample_size(20);
    // Throughput in seconds of audio: criterion's elements/s is the
    // real-time factor.
    group.throughput(Throughput::Elements(u64::from(SECONDS)));
    for rate in [48_000, 44_100] {
        let pcm = signal(rate);
        group.bench_function(format!("encode_10s_{rate}hz"), |b| {
            b.iter(|| {
                let mut encoder = VoiceEncoder::new(rate, |_| {}).unwrap();
                for block in pcm.chunks(rate as usize / 100) {
                    encoder.push(block).unwrap();
                }
                encoder.finish().unwrap()
            });
        });
    }
    group.finish();
}

fn webm(c: &mut Criterion) {
    let packet = [0x5Au8; 80];
    c.bench_function("webm/mux_60s", |b| {
        b.iter(|| {
            let mut mux = OpusWebm::new(312);
            for _ in 0..3_000 {
                mux.push(&packet);
            }
            mux.finish()
        });
    });
}

criterion_group!(benches, voice, webm);
criterion_main!(benches);
