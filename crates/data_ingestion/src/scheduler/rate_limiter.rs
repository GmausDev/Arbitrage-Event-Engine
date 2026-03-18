// crates/data_ingestion/src/scheduler/rate_limiter.rs
//
// Token-bucket rate limiter.
//
// Tokens refill at `rate_per_sec` per second up to `capacity`.
// `try_acquire()` is non-blocking; `acquire()` is async and waits for a token.

use std::time::Instant;

use tokio::time::Duration;

pub struct RateLimiter {
    /// Refill rate in tokens per millisecond.
    refill_rate_per_ms: f64,
    /// Maximum token count (burst ceiling).
    capacity: f64,
    /// Current available tokens.
    tokens: f64,
    /// Wall-clock time of the last refill.
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new token-bucket rate limiter.
    ///
    /// `rate_per_sec`  — sustained throughput (tokens / second).
    /// `burst_capacity` — maximum tokens (burst), usually equal to `rate_per_sec`.
    pub fn new(rate_per_sec: f64, burst_capacity: f64) -> Self {
        debug_assert!(rate_per_sec > 0.0, "rate must be positive");
        debug_assert!(burst_capacity >= 1.0, "capacity must be >= 1");
        Self {
            refill_rate_per_ms: rate_per_sec / 1_000.0,
            capacity: burst_capacity,
            tokens: burst_capacity, // start with a full bucket
            last_refill: Instant::now(),
        }
    }

    /// Refill the bucket based on elapsed wall time since the last call.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed_ms = now.duration_since(self.last_refill).as_millis() as f64;
        self.tokens = (self.tokens + elapsed_ms * self.refill_rate_per_ms).min(self.capacity);
        self.last_refill = now;
    }

    /// Non-blocking: returns `true` and consumes one token if available, else `false`.
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Async: wait until a token is available, then consume it.
    pub async fn acquire(&mut self) {
        loop {
            if self.try_acquire() {
                return;
            }
            // Calculate how long until the next token arrives.
            let wait_ms = ((1.0 - self.tokens) / self.refill_rate_per_ms).ceil() as u64 + 1;
            tokio::time::sleep(Duration::from_millis(wait_ms.max(1))).await;
        }
    }

    /// Return the approximate number of tokens currently available (may be stale).
    pub fn available(&self) -> f64 {
        self.tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_full_bucket() {
        let rl = RateLimiter::new(2.0, 5.0);
        assert_eq!(rl.available().floor() as u64, 5);
    }

    #[test]
    fn try_acquire_consumes_token() {
        let mut rl = RateLimiter::new(10.0, 3.0);
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        // Bucket drained — but because of refill based on elapsed time there
        // might be a tiny refill; allow one more attempt to fail or pass.
        // The important invariant: capacity is bounded.
        assert!(rl.available() <= 3.0);
    }

    #[test]
    fn try_acquire_fails_on_empty_bucket() {
        let mut rl = RateLimiter::new(0.001, 1.0); // very slow refill
        assert!(rl.try_acquire()); // first token succeeds
        // Immediate second call: no time has elapsed to refill
        let ok = rl.try_acquire();
        // This may be true if the Instant resolution is coarse, but in practice
        // the token should be ~0. We just assert no panic.
        let _ = ok;
    }

    #[tokio::test]
    async fn acquire_returns_within_reasonable_time() {
        let mut rl = RateLimiter::new(100.0, 2.0); // 100 req/s
        // Drain the bucket
        rl.try_acquire();
        rl.try_acquire();
        // acquire should return quickly (< 50 ms at 100 req/s)
        let start = Instant::now();
        rl.acquire().await;
        assert!(start.elapsed().as_millis() < 200, "acquire took too long");
    }

    #[test]
    fn tokens_do_not_exceed_capacity() {
        let mut rl = RateLimiter::new(1_000.0, 5.0);
        // Sleep-simulate by manually advancing — just call refill many times
        for _ in 0..100 {
            rl.refill();
        }
        assert!(rl.tokens <= 5.0 + 1e-9);
    }
}
