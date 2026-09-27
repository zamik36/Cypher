use std::time::Instant;

/// Token bucket with a burst capacity and a steady refill rate.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    rate: f64,
    last: Instant,
}

impl TokenBucket {
    #[expect(
        clippy::cast_precision_loss,
        reason = "configured rates and sizes are far below 2^52"
    )]
    pub fn new(capacity: u64, per_second: u64) -> Self {
        Self {
            capacity: capacity as f64,
            tokens: capacity as f64,
            rate: per_second as f64,
            last: Instant::now(),
        }
    }

    pub fn try_consume(&mut self, n: u64) -> bool {
        self.try_consume_at(n, Instant::now())
    }

    #[expect(clippy::cast_precision_loss, reason = "frame sizes are far below 2^52")]
    fn try_consume_at(&mut self, n: u64, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last = now;
        let cost = n as f64;
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

/// Limits both message rate and bandwidth of one connection.
#[derive(Debug)]
pub struct ConnLimiter {
    frames: TokenBucket,
    bytes: TokenBucket,
}

impl ConnLimiter {
    pub fn new(frames_per_sec: u64, bytes_per_sec: u64) -> Self {
        Self {
            frames: TokenBucket::new(frames_per_sec * 2, frames_per_sec),
            bytes: TokenBucket::new(bytes_per_sec * 2, bytes_per_sec),
        }
    }

    pub fn admit(&mut self, len: usize) -> bool {
        self.frames.try_consume(1) && self.bytes.try_consume(len as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn burst_then_refill() {
        let start = Instant::now();
        let mut b = TokenBucket::new(5, 10);
        b.last = start;
        for _ in 0..5 {
            assert!(b.try_consume_at(1, start));
        }
        assert!(!b.try_consume_at(1, start));
        assert!(b.try_consume_at(1, start + Duration::from_millis(100)));
    }

    #[test]
    fn never_exceeds_capacity() {
        let start = Instant::now();
        let mut b = TokenBucket::new(3, 1000);
        b.last = start;
        assert!(b.try_consume_at(3, start + Duration::from_secs(60)));
        assert!(!b.try_consume_at(1, start + Duration::from_secs(60)));
    }

    #[test]
    fn conn_limiter_caps_bytes() {
        let mut l = ConnLimiter::new(1000, 100);
        assert!(l.admit(150));
        assert!(!l.admit(100));
    }
}
