use std::time::Instant;

/// A token-bucket rate limiter.
///
/// Tokens are consumed by calls to [`try_consume`](TokenBucket::try_consume)
/// and automatically refilled based on elapsed time.
pub struct TokenBucket {
    capacity: u32,
    tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, refill_rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity as f64,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Refill tokens based on elapsed time since the last refill.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity as f64);
        self.last_refill = now;
    }

    /// Try to consume `n` tokens. Returns `true` if successful.
    pub fn try_consume(&mut self, n: u32) -> bool {
        self.refill();
        let cost = n as f64;
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn initial_burst() {
        let mut bucket = TokenBucket::new(10, 1.0);
        for _ in 0..10 {
            assert!(bucket.try_consume(1));
        }
        assert!(!bucket.try_consume(1));
    }

    #[test]
    fn refill_after_time() {
        let mut bucket = TokenBucket::new(5, 100.0);
        // Exhaust all tokens.
        for _ in 0..5 {
            assert!(bucket.try_consume(1));
        }
        assert!(!bucket.try_consume(1));
        // Wait a bit for refill.
        thread::sleep(Duration::from_millis(60));
        assert!(bucket.try_consume(1));
    }

    #[test]
    fn does_not_exceed_capacity() {
        let mut bucket = TokenBucket::new(5, 1000.0);
        thread::sleep(Duration::from_millis(50));
        // Even after generous refill, cannot consume more than capacity.
        assert!(bucket.try_consume(5));
        assert!(!bucket.try_consume(1));
    }
}
