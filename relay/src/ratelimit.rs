//! Coarse per-connection rate limiter to blunt control-frame flooding at the
//! relay edge (SEC-007). This complements — it does not replace — the business
//! `CMD_RATE_LIMIT` enforced at the Host (BR-017).
//!
//! Implemented as a fixed-window counter: cheap, allocation-free, and good
//! enough for anti-flood. It is intentionally not a precise token bucket.

use std::time::{Duration, Instant};

/// Fixed-window rate limiter for a single connection.
pub struct RateLimiter {
    max: u32,
    window: Duration,
    window_start: Instant,
    count: u32,
}

impl RateLimiter {
    /// Create a limiter allowing `max` events per `window`.
    pub fn new(max: u32, window: Duration, now: Instant) -> Self {
        RateLimiter {
            max,
            window,
            window_start: now,
            count: 0,
        }
    }

    /// Record one event. Returns `true` if it is within budget, `false` if it
    /// exceeds the limit for the current window.
    pub fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window_start) >= self.window {
            self.window_start = now;
            self.count = 0;
        }
        if self.count >= self.max {
            return false;
        }
        self.count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_max_then_blocks() {
        let now = Instant::now();
        let mut rl = RateLimiter::new(3, Duration::from_secs(10), now);
        assert!(rl.allow(now));
        assert!(rl.allow(now));
        assert!(rl.allow(now));
        assert!(!rl.allow(now), "4th within window must be blocked");
    }

    #[test]
    fn resets_after_window() {
        let now = Instant::now();
        let mut rl = RateLimiter::new(1, Duration::from_secs(10), now);
        assert!(rl.allow(now));
        assert!(!rl.allow(now));
        let later = now + Duration::from_secs(11);
        assert!(rl.allow(later), "budget must reset after the window");
    }
}
