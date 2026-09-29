//! Microphone capture. `cpal` streams are not `Send` on every platform, so
//! one dedicated thread owns the stream and encodes: the audio callback only
//! downmixes into a lock-free SPSC ring, never allocating or blocking.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::voice::{Recording, VoiceEncoder};
use crate::{MediaError, num};

/// How often the encoder thread drains the ring.
const POLL: Duration = Duration::from_millis(20);
/// Ring capacity in seconds of audio: absorbs encoder hiccups.
const RING_SECONDS: usize = 2;

/// A running source (kept alive while held), its sample rate and the ring
/// its mono samples arrive in.
type Source<S> = (S, u32, Consumer<f32>);

pub struct Recorder {
    stop: mpsc::Sender<bool>,
    worker: JoinHandle<Result<Option<Recording>, MediaError>>,
}

impl Recorder {
    /// Opens the default microphone and starts encoding. Returns once the
    /// device is running, or with the reason it could not be opened.
    pub fn start(on_level: impl FnMut(f32) + Send + 'static) -> Result<Self, MediaError> {
        Self::start_with(open_input, on_level)
    }

    /// `open` runs on the encoder thread, which then owns what it returns.
    fn start_with<S>(
        open: impl FnOnce() -> Result<Source<S>, MediaError> + Send + 'static,
        on_level: impl FnMut(f32) + Send + 'static,
    ) -> Result<Self, MediaError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop, stop_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("cypher-voice".into())
            .spawn(move || run(open, on_level, &ready_tx, &stop_rx))
            .map_err(|e| MediaError::Device(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, worker }),
            Ok(Err(e)) => {
                let _ = worker.join();
                Err(e)
            }
            Err(_) => Err(MediaError::Aborted),
        }
    }

    pub fn finish(self) -> Result<Recording, MediaError> {
        let _ = self.stop.send(true);
        self.worker
            .join()
            .map_err(|_| MediaError::Aborted)?
            .and_then(|r| r.ok_or(MediaError::Aborted))
    }

    pub fn cancel(self) {
        let _ = self.stop.send(false);
        let _ = self.worker.join();
    }
}

#[cfg(feature = "test-util")]
impl Recorder {
    /// A recorder that hears `samples` (mono, at `rate`) instead of the
    /// microphone.
    pub fn from_samples(rate: u32, samples: &[f32]) -> Result<Self, MediaError> {
        let (mut producer, consumer) = RingBuffer::new(samples.len().max(1));
        for &sample in samples {
            producer.push(sample).map_err(|_| MediaError::Aborted)?;
        }
        Self::start_with(move || Ok((producer, rate, consumer)), |_| {})
    }
}

fn run<S>(
    open: impl FnOnce() -> Result<Source<S>, MediaError>,
    on_level: impl FnMut(f32),
    ready: &mpsc::SyncSender<Result<(), MediaError>>,
    stop: &mpsc::Receiver<bool>,
) -> Result<Option<Recording>, MediaError> {
    let opened = open()
        .and_then(|(source, rate, ring)| Ok((source, ring, VoiceEncoder::new(rate, on_level)?)));
    let (source, mut ring, mut encoder) = match opened {
        Ok(parts) => {
            let _ = ready.send(Ok(()));
            parts
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return Ok(None);
        }
    };

    let keep = loop {
        match stop.recv_timeout(POLL) {
            Ok(keep) => break keep,
            Err(RecvTimeoutError::Timeout) => drain(&mut ring, &mut encoder)?,
            Err(RecvTimeoutError::Disconnected) => break false,
        }
    };
    drop(source);
    if !keep {
        return Ok(None);
    }
    drain(&mut ring, &mut encoder)?;
    encoder.finish().map(Some)
}

fn drain<L: FnMut(f32)>(
    ring: &mut Consumer<f32>,
    encoder: &mut VoiceEncoder<L>,
) -> Result<(), MediaError> {
    let n = ring.slots();
    if n == 0 {
        return Ok(());
    }
    let Ok(chunk) = ring.read_chunk(n) else {
        return Ok(());
    };
    let (a, b) = chunk.as_slices();
    encoder.push(a)?;
    encoder.push(b)?;
    chunk.commit_all();
    Ok(())
}

fn open_input() -> Result<Source<Stream>, MediaError> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or(MediaError::NoInputDevice)?;
    let supported = device
        .default_input_config()
        .map_err(|e| MediaError::Device(e.to_string()))?;
    let config = supported.config();
    let rate = config.sample_rate;
    let (producer, consumer) = RingBuffer::new(rate as usize * RING_SECONDS);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, producer),
        SampleFormat::I16 => build::<i16>(&device, &config, producer),
        SampleFormat::I32 => build::<i32>(&device, &config, producer),
        SampleFormat::U16 => build::<u16>(&device, &config, producer),
        SampleFormat::U8 => build::<u8>(&device, &config, producer),
        _ => Err(MediaError::UnsupportedFormat),
    }?;
    stream
        .play()
        .map_err(|e| MediaError::Device(e.to_string()))?;
    Ok((stream, rate, consumer))
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut ring: Producer<f32>,
) -> Result<Stream, MediaError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _| downmix(data, channels, &mut ring),
            // A device error (e.g. unplugged mic) just ends the audio; the
            // recording keeps what was captured so far.
            |_| {},
            None,
        )
        .map_err(|e| MediaError::Device(e.to_string()))
}

/// Averages each interleaved frame to mono. A full ring means the encoder
/// stalled; dropping audio beats blocking the device callback.
fn downmix<T>(data: &[T], channels: usize, ring: &mut Producer<f32>)
where
    T: Copy,
    f32: FromSample<T>,
{
    let scale = 1.0 / num::count(channels);
    for frame in data.chunks_exact(channels) {
        let mono: f32 = frame.iter().map(|&s| f32::from_sample_(s)).sum::<f32>() * scale;
        let _ = ring.push(mono);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::SAMPLE_RATE;

    fn drained(mut ring: Consumer<f32>) -> Vec<f32> {
        std::iter::from_fn(|| ring.pop().ok()).collect()
    }

    #[test]
    fn downmix_averages_interleaved_channels() {
        let (mut producer, consumer) = RingBuffer::new(8);
        downmix(&[i16::MAX, i16::MAX, i16::MAX, 0, 0, 0], 2, &mut producer);
        let mono = drained(consumer);
        assert_eq!(mono.len(), 3);
        assert!((mono[0] - 1.0).abs() < 1e-3 && (mono[1] - 0.5).abs() < 1e-3);
        assert!(mono[2].abs() < f32::EPSILON);
    }

    #[test]
    fn full_ring_drops_audio_instead_of_blocking() {
        let (mut producer, consumer) = RingBuffer::new(2);
        downmix(&[0.1_f32, 0.2, 0.3, 0.4], 1, &mut producer);
        assert_eq!(drained(consumer), [0.1, 0.2]);
    }

    /// A source that is "running" until dropped, fed by the test.
    fn fake_source() -> (
        Producer<f32>,
        impl FnOnce() -> Result<Source<()>, MediaError>,
    ) {
        let (producer, consumer) = RingBuffer::new(SAMPLE_RATE as usize * RING_SECONDS);
        (producer, move || Ok(((), SAMPLE_RATE, consumer)))
    }

    fn tone(producer: &mut Producer<f32>, samples: usize) {
        for i in 0..samples {
            producer.push(if i % 48 < 24 { 0.5 } else { -0.5 }).unwrap();
        }
    }

    #[test]
    fn finish_encodes_everything_captured() {
        let (mut producer, open) = fake_source();
        let levels = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&levels);
        let recorder = Recorder::start_with(open, move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        tone(&mut producer, SAMPLE_RATE as usize);
        let recording = recorder.finish().unwrap();
        assert!(
            (980..=1_020).contains(&recording.duration_ms),
            "{}",
            recording.duration_ms
        );
        assert!(!recording.webm.is_empty() && !recording.waveform.is_empty());
        assert!(
            levels.load(Ordering::Relaxed) > 0,
            "the level meter was fed"
        );
    }

    #[test]
    fn cancel_discards_the_recording() {
        let (mut producer, open) = fake_source();
        let recorder = Recorder::start_with(open, |_| {}).unwrap();
        tone(&mut producer, 4_800);
        recorder.cancel();
    }

    #[test]
    fn open_failures_are_returned_by_start() {
        let open = || -> Result<Source<()>, MediaError> { Err(MediaError::NoInputDevice) };
        assert!(matches!(
            Recorder::start_with(open, |_| {}),
            Err(MediaError::NoInputDevice)
        ));
    }
}
