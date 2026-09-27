//! Numeric conversions of the audio code. Sample counts, rates and levels
//! cross between integer and float types; every lossy step lives here, with
//! the reason it is harmless for audio.

/// `num / den` for sample rates.
#[cfg(feature = "opus")]
#[expect(
    clippy::cast_precision_loss,
    reason = "audio sample rates are far below 2^24, exact in f32"
)]
pub(crate) fn ratio(num: u32, den: u32) -> f32 {
    num as f32 / den.max(1) as f32
}

/// A sample or channel count used as a divisor.
#[expect(
    clippy::cast_precision_loss,
    reason = "audio buffers hold far fewer than 2^24 samples"
)]
pub(crate) fn count(n: usize) -> f32 {
    n as f32
}

/// A `[-1, 1]` float sample as 16-bit PCM.
#[cfg(feature = "opus")]
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to [-1, 1], so the product fits in i16"
)]
pub(crate) fn pcm16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

/// A `0..=1` level as a waveform bar height.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 before the cast"
)]
pub(crate) fn bar(level: f32) -> u8 {
    (level.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_clamp_and_scale() {
        assert!((count(960) - 960.0).abs() < f32::EPSILON);
        assert_eq!((bar(-1.0), bar(0.5), bar(7.0)), (0, 128, 255));
    }

    #[cfg(feature = "opus")]
    #[test]
    fn rate_ratio_is_exact_and_never_divides_by_zero() {
        assert!((ratio(44_100, 48_000) - 0.918_75).abs() < 1e-6);
        assert!(ratio(1, 0).is_finite());
    }

    #[cfg(feature = "opus")]
    #[test]
    fn pcm_saturates_at_full_scale() {
        assert_eq!(
            (pcm16(-2.0), pcm16(0.0), pcm16(2.0)),
            (-i16::MAX, 0, i16::MAX)
        );
    }
}
