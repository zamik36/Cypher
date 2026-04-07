use std::time::Duration;

use rand::Rng;

/// Generate a single random jitter delay using a truncated exponential
/// distribution (mean ≈ 100ms, clamped to \[20ms, 500ms\]).
pub fn next_jitter() -> Duration {
    let mut rng = rand::thread_rng();
    let u: f64 = rng.gen_range(0.0001..1.0); // avoid ln(0)
    let raw = -100.0 * u.ln();
    let clamped = raw.clamp(20.0, 500.0);
    Duration::from_millis(clamped as u64)
}

/// Build a jitter schedule for `n_requests` pipelined fetches.
///
/// Returns a `Vec<Duration>` of length `n_requests`, each being a random
/// exponential delay. Additionally inserts 1–2 longer pauses (300–1000ms) at
/// random positions to defeat timing fingerprinting.
pub fn pipeline_schedule(n_requests: usize) -> Vec<Duration> {
    if n_requests == 0 {
        return Vec::new();
    }
    let mut rng = rand::thread_rng();
    let mut delays: Vec<Duration> = (0..n_requests).map(|_| next_jitter()).collect();

    let pause_count = rng.gen_range(1..=2);
    for _ in 0..pause_count {
        let pos = rng.gen_range(0..n_requests);
        let extra = Duration::from_millis(rng.gen_range(300..=1000));
        delays[pos] += extra;
    }
    delays
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_within_bounds() {
        for _ in 0..1000 {
            let d = next_jitter();
            assert!(d >= Duration::from_millis(20));
            assert!(d <= Duration::from_millis(500));
        }
    }

    #[test]
    fn schedule_length() {
        let s = pipeline_schedule(9);
        assert_eq!(s.len(), 9);
    }

    #[test]
    fn schedule_has_long_pause() {
        // Over 100 trials, at least one schedule should contain a delay > 500ms
        // (from the injected long pause).
        let has_long = (0..100).any(|_| {
            pipeline_schedule(9)
                .iter()
                .any(|d| *d > Duration::from_millis(500))
        });
        assert!(has_long);
    }

    #[test]
    fn schedule_empty() {
        assert!(pipeline_schedule(0).is_empty());
    }
}
