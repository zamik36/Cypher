//! Voice-note media: waveform previews and a WebM/Opus muxer everywhere,
//! plus Opus encoding (`opus`) and microphone capture (`capture`) natively.

mod num;
pub mod waveform;
pub mod webm;

#[cfg(feature = "capture")]
mod capture;
#[cfg(feature = "opus")]
mod opus;
#[cfg(feature = "opus")]
mod voice;

#[cfg(feature = "capture")]
pub use capture::Recorder;
#[cfg(feature = "opus")]
pub use voice::{Recording, VoiceEncoder};

/// Opus works at 48 kHz; every voice frame is 20 ms of mono audio.
pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;
/// Longest voice note; capture stops accepting audio beyond it.
pub const MAX_VOICE_MS: u32 = 15 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("no microphone available")]
    NoInputDevice,
    #[error("audio device error: {0}")]
    Device(String),
    #[error("unsupported sample format")]
    UnsupportedFormat,
    #[error("opus encoder error {0}")]
    Encoder(i32),
    #[error("recording thread stopped unexpectedly")]
    Aborted,
}
