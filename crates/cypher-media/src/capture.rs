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

pub struct Recorder {
    stop: mpsc::Sender<bool>,
    worker: JoinHandle<Result<Option<Recording>, MediaError>>,
}

impl Recorder {
    /// Opens the default microphone and starts encoding. Returns once the
    /// device is running, or with the reason it could not be opened.
    pub fn start(on_level: impl FnMut(f32) + Send + 'static) -> Result<Self, MediaError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop, stop_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("cypher-voice".into())
            .spawn(move || run(on_level, &ready_tx, &stop_rx))
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

fn run(
    on_level: impl FnMut(f32),
    ready: &mpsc::SyncSender<Result<(), MediaError>>,
    stop: &mpsc::Receiver<bool>,
) -> Result<Option<Recording>, MediaError> {
    let opened = open_input().and_then(|(stream, rate, ring)| {
        let encoder = VoiceEncoder::new(rate, on_level)?;
        stream
            .play()
            .map_err(|e| MediaError::Device(e.to_string()))?;
        Ok((stream, ring, encoder))
    });
    let (stream, mut ring, mut encoder) = match opened {
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
    drop(stream);
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

fn open_input() -> Result<(Stream, u32, Consumer<f32>), MediaError> {
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
    let scale = 1.0 / num::count(channels);
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _| {
                for frame in data.chunks_exact(channels) {
                    let mono: f32 =
                        frame.iter().map(|&s| f32::from_sample_(s)).sum::<f32>() * scale;
                    // A full ring means the encoder stalled; dropping audio
                    // beats blocking the device callback.
                    let _ = ring.push(mono);
                }
            },
            // A device error (e.g. unplugged mic) just ends the audio; the
            // recording keeps what was captured so far.
            |_| {},
            None,
        )
        .map_err(|e| MediaError::Device(e.to_string()))
}
