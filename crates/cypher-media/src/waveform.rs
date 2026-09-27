//! Loudness summaries: a live level meter and the fixed-size waveform that
//! travels inside a voice message.

/// Buckets in a voice-message waveform.
pub const BUCKETS: usize = 64;
/// Levels below this are drawn as silence.
const FLOOR_DB: f32 = -60.0;

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

/// Perceptual 0..=1 level of an RMS value, for recording meters.
pub fn level(rms: f32) -> f32 {
    if rms <= 0.0 {
        return 0.0;
    }
    ((20.0 * rms.log10() - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// Reduces per-frame RMS values to [`BUCKETS`] bars scaled to the loudest.
pub fn from_frame_rms(frames: &[f32]) -> Vec<u8> {
    if frames.is_empty() {
        return vec![0; BUCKETS];
    }
    let peaks: Vec<f32> = (0..BUCKETS)
        .map(|b| {
            let start = b * frames.len() / BUCKETS;
            let end = ((b + 1) * frames.len() / BUCKETS).max(start + 1);
            frames[start.min(frames.len() - 1)..end.min(frames.len())]
                .iter()
                .copied()
                .fold(0.0, f32::max)
        })
        .collect();
    let loudest = peaks.iter().copied().fold(0.0, f32::max);
    if loudest <= f32::EPSILON {
        return vec![0; BUCKETS];
    }
    peaks
        .iter()
        .map(|p| ((p / loudest).sqrt() * 255.0).round() as u8)
        .collect()
}

/// Waveform of decoded mono PCM, e.g. a voice note recorded in the browser.
pub fn from_pcm(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let frame = (sample_rate as usize / 50).max(1);
    let frames: Vec<f32> = samples.chunks(frame).map(rms).collect();
    from_frame_rms(&frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_and_empty_input_are_flat() {
        assert_eq!(from_frame_rms(&[]), vec![0; BUCKETS]);
        assert_eq!(from_pcm(&[0.0; 48_000], 48_000), vec![0; BUCKETS]);
        assert_eq!(level(0.0), 0.0);
    }

    #[test]
    fn loud_half_shows_up_in_the_right_buckets() {
        let mut pcm = vec![0.0f32; 48_000];
        for (i, s) in pcm[24_000..].iter_mut().enumerate() {
            *s = (i as f32 * 0.05).sin() * 0.5;
        }
        let w = from_pcm(&pcm, 48_000);
        assert_eq!(w.len(), BUCKETS);
        assert!(w[..BUCKETS / 2 - 1].iter().all(|&v| v == 0));
        assert!(w[BUCKETS / 2 + 1..].iter().all(|&v| v > 200));
    }

    #[test]
    fn short_input_still_fills_every_bucket() {
        let w = from_frame_rms(&[0.1, 0.5, 0.2]);
        assert_eq!(w.len(), BUCKETS);
        assert_eq!(w.iter().copied().max(), Some(255));
    }

    #[test]
    fn level_maps_dbfs_to_unit_range() {
        assert_eq!(level(1.0), 1.0);
        assert!((level(0.001) - 0.0).abs() < 1e-6);
        assert!((level(0.031_62) - 0.5).abs() < 0.01);
    }
}
