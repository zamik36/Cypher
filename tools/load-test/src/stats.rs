use std::time::Duration;

use serde::Serialize;

pub(crate) fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// Nearest-rank percentiles of a sample set.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Percentiles {
    pub p50: u64,
    pub p90: u64,
    pub p99: u64,
    pub p999: u64,
    pub max: u64,
}

impl Percentiles {
    pub(crate) fn of(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        let at = |per_mille: usize| {
            let rank = samples.len().saturating_mul(per_mille) / 1000;
            samples
                .get(rank.min(samples.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        Self {
            p50: at(500),
            p90: at(900),
            p99: at(990),
            p999: at(999),
            max: samples.last().copied().unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let p = Percentiles::of((1..=1000).rev().collect());
        assert_eq!(
            p,
            Percentiles {
                p50: 501,
                p90: 901,
                p99: 991,
                p999: 1000,
                max: 1000
            }
        );
        assert_eq!(Percentiles::of(vec![7]).p999, 7);
        assert_eq!(Percentiles::of(Vec::new()), Percentiles::default());
    }

    #[test]
    fn durations_saturate() {
        assert_eq!(micros(Duration::from_millis(3)), 3_000);
        assert_eq!(micros(Duration::MAX), u64::MAX);
    }
}
