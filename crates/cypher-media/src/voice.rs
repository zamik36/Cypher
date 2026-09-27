//! Device-independent voice pipeline: mono PCM at any rate in, a finished
//! WebM/Opus file with its waveform out.

use crate::opus::{MAX_PACKET, OpusEncoder};
use crate::webm::OpusWebm;
use crate::{FRAME_MS, FRAME_SAMPLES, MAX_VOICE_MS, MediaError, SAMPLE_RATE, num, waveform};

/// Speech bitrate: transparent for voice, ~180 KiB per minute.
const BITRATE: i32 = 24_000;
/// Level callbacks fire every other frame (25 Hz), enough for a meter.
const LEVEL_EVERY_FRAMES: usize = 2;

pub struct Recording {
    pub webm: Vec<u8>,
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
}

pub struct VoiceEncoder<L> {
    resampler: Resampler,
    resampled: Vec<f32>,
    frame: Vec<f32>,
    sink: FrameSink<L>,
}

/// Everything that consumes complete 20 ms frames.
struct FrameSink<L> {
    pcm: [i16; FRAME_SAMPLES],
    packet: [u8; MAX_PACKET],
    encoder: OpusEncoder,
    webm: OpusWebm,
    frame_rms: Vec<f32>,
    on_level: L,
}

impl<L: FnMut(f32)> VoiceEncoder<L> {
    /// `input_rate` is the capture device's rate; `on_level` receives a 0..=1
    /// loudness value for recording meters.
    pub fn new(input_rate: u32, on_level: L) -> Result<Self, MediaError> {
        let encoder = OpusEncoder::voice(BITRATE)?;
        let webm = OpusWebm::new(encoder.lookahead()?);
        Ok(Self {
            resampler: Resampler::new(input_rate),
            resampled: Vec::with_capacity(4096),
            frame: Vec::with_capacity(FRAME_SAMPLES),
            sink: FrameSink {
                pcm: [0; FRAME_SAMPLES],
                packet: [0; MAX_PACKET],
                encoder,
                webm,
                frame_rms: Vec::new(),
                on_level,
            },
        })
    }

    pub fn duration_ms(&self) -> u32 {
        u32::try_from(self.sink.frame_rms.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(FRAME_MS)
    }

    /// Feeds mono samples in `[-1, 1]`; audio past [`MAX_VOICE_MS`] is dropped.
    pub fn push(&mut self, mono: &[f32]) -> Result<(), MediaError> {
        self.resampled.clear();
        self.resampler.process(mono, &mut self.resampled);
        let mut rest = self.resampled.as_slice();
        while !rest.is_empty() && self.duration_ms() < MAX_VOICE_MS {
            let take = (FRAME_SAMPLES - self.frame.len()).min(rest.len());
            let (head, tail) = rest.split_at(take);
            self.frame.extend_from_slice(head);
            rest = tail;
            if self.frame.len() == FRAME_SAMPLES {
                self.sink.encode(&self.frame)?;
                self.frame.clear();
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Recording, MediaError> {
        if !self.frame.is_empty() {
            self.frame.resize(FRAME_SAMPLES, 0.0);
            self.sink.encode(&self.frame)?;
        }
        Ok(Recording {
            duration_ms: self.duration_ms(),
            waveform: waveform::from_frame_rms(&self.sink.frame_rms),
            webm: self.sink.webm.finish(),
        })
    }
}

impl<L: FnMut(f32)> FrameSink<L> {
    fn encode(&mut self, frame: &[f32]) -> Result<(), MediaError> {
        let rms = waveform::rms(frame);
        for (dst, &s) in self.pcm.iter_mut().zip(frame) {
            *dst = num::pcm16(s);
        }
        let n = self.encoder.encode(&self.pcm, &mut self.packet)?;
        let packet = self.packet.get(..n).ok_or(MediaError::Encoder(-1))?;
        self.webm.push(packet);
        self.frame_rms.push(rms);
        if self.frame_rms.len().is_multiple_of(LEVEL_EVERY_FRAMES) {
            (self.on_level)(waveform::level(rms));
        }
        Ok(())
    }
}

/// Linear-interpolating converter to 48 kHz. Opus in VOIP mode at 24 kbit/s
/// codes at most wideband speech, so interpolation images above ~12 kHz are
/// discarded by the encoder anyway; most devices already run at 48 kHz.
struct Resampler {
    /// Input samples consumed per output sample; `None` passes 48 kHz through.
    step: Option<f32>,
    /// Position in `[last] ++ input`: index 0 is the previous call's last
    /// sample, so interpolation continues seamlessly across calls.
    index: usize,
    frac: f32,
    last: f32,
}

impl Resampler {
    fn new(input_rate: u32) -> Self {
        Self {
            step: (input_rate != SAMPLE_RATE).then(|| num::ratio(input_rate, SAMPLE_RATE)),
            index: 1,
            frac: 0.0,
            last: 0.0,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let Some(step) = self.step else {
            out.extend_from_slice(input);
            return;
        };
        let Some(&tail) = input.last() else {
            return;
        };
        while let Some(&b) = input.get(self.index) {
            let a = self
                .index
                .checked_sub(1)
                .and_then(|i| input.get(i))
                .copied()
                .unwrap_or(self.last);
            out.push(a + (b - a) * self.frac);
            self.frac += step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                self.index += 1;
            }
        }
        self.index -= input.len();
        self.last = tail;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use matroska_demuxer::{Frame, MatroskaFile};

    use super::*;

    /// `samples` of a 440 Hz sine at `rate`.
    fn tone(rate: u32, samples: usize) -> Vec<f32> {
        let step = num::ratio(440, rate) * std::f32::consts::TAU;
        let mut phase = 0.0f32;
        (0..samples)
            .map(|_| {
                phase += step;
                phase.sin() * 0.4
            })
            .collect()
    }

    #[test]
    fn resampler_preserves_duration_across_chunk_boundaries() {
        for rate in [8_000, 16_000, 44_100, 48_000, 96_000] {
            let input = tone(rate, rate as usize);
            let mut r = Resampler::new(rate);
            let mut out = Vec::new();
            for chunk in input.chunks(333) {
                r.process(chunk, &mut out);
            }
            // Interpolation holds back at most one input sample's worth of
            // output until the next chunk arrives.
            let slack = (SAMPLE_RATE / rate) as usize + 1;
            let expected = SAMPLE_RATE as usize;
            assert!(
                out.len().abs_diff(expected) <= slack,
                "{rate}: {}",
                out.len()
            );
        }
    }

    #[test]
    fn encodes_a_playable_voice_note() {
        let mut levels = Vec::new();
        let mut enc = VoiceEncoder::new(44_100, |l| levels.push(l)).unwrap();
        let mut pcm = tone(44_100, 66_150);
        pcm.extend(std::iter::repeat_n(0.0, 44_100 / 2));
        for chunk in pcm.chunks(441) {
            enc.push(chunk).unwrap();
        }
        let rec = enc.finish().unwrap();

        assert_eq!(rec.duration_ms, 2000);
        assert_eq!(rec.waveform.len(), waveform::BUCKETS);
        assert!(rec.waveform[..40].iter().all(|&v| v > 200));
        assert!(rec.waveform[52..].iter().all(|&v| v < 30));
        assert_eq!(levels.len(), 50);
        assert!(rec.webm.len() < 10 * 1024, "24 kbit/s: {}", rec.webm.len());

        let mut mkv = MatroskaFile::open(Cursor::new(rec.webm)).unwrap();
        assert_eq!(mkv.info().duration(), Some(2000.0));
        let mut frame = Frame::default();
        let mut frames = 0;
        while mkv.next_frame(&mut frame).unwrap() {
            assert!(!frame.data.is_empty());
            frames += 1;
        }
        assert_eq!(frames, 100);
    }

    #[test]
    fn stops_at_the_length_limit() {
        let mut enc = VoiceEncoder::new(48_000, |_| {}).unwrap();
        let second = vec![0.0f32; 48_000];
        for _ in 0..(MAX_VOICE_MS / 1000 + 5) {
            enc.push(&second).unwrap();
        }
        assert_eq!(enc.duration_ms(), MAX_VOICE_MS);
    }
}
