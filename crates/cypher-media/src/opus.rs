//! Speech encoder over the reference libopus (C, bundled and built with
//! cmake). The `opus` crate owns every FFI call, so this crate has no unsafe.

use opus::{Application, Bitrate, Channels, Signal};

use crate::{FRAME_SAMPLES, MediaError, SAMPLE_RATE};

/// Largest packet libopus produces for one frame (RFC 6716 §3.2.1).
pub(crate) const MAX_PACKET: usize = 1275;

pub(crate) struct OpusEncoder(opus::Encoder);

impl OpusEncoder {
    /// 48 kHz mono encoder tuned for speech at `bitrate` bits per second.
    pub(crate) fn voice(bitrate: i32) -> Result<Self, MediaError> {
        let mut encoder = opus::Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .map_err(|e| codec(&e))?;
        encoder
            .set_bitrate(Bitrate::Bits(bitrate))
            .map_err(|e| codec(&e))?;
        encoder.set_signal(Signal::Voice).map_err(|e| codec(&e))?;
        Ok(Self(encoder))
    }

    /// Encoder delay in 48 kHz samples: the Ogg/WebM `pre_skip`.
    pub(crate) fn lookahead(&mut self) -> Result<u16, MediaError> {
        let samples = self.0.get_lookahead().map_err(|e| codec(&e))?;
        u16::try_from(samples).map_err(|_| MediaError::Encoder(format!("lookahead {samples}")))
    }

    /// Encodes exactly one 20 ms frame into `out`, returning the packet size.
    pub(crate) fn encode(
        &mut self,
        pcm: &[i16; FRAME_SAMPLES],
        out: &mut [u8; MAX_PACKET],
    ) -> Result<usize, MediaError> {
        self.0.encode(pcm, out).map_err(|e| codec(&e))
    }
}

fn codec(e: &opus::Error) -> MediaError {
    MediaError::Encoder(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_one_frame() {
        let mut encoder = OpusEncoder::voice(24_000).unwrap();
        assert!(encoder.lookahead().unwrap() > 0);
        let mut pcm = [0i16; FRAME_SAMPLES];
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = if i % 48 < 24 { 8_000 } else { -8_000 };
        }
        let mut packet = [0u8; MAX_PACKET];
        let n = encoder.encode(&pcm, &mut packet).unwrap();
        assert!((1..=MAX_PACKET).contains(&n));
    }
}
