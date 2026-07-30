//! Minimal in-process per-key rate limiter (fixed window).
//!
//! Re-adds brute-force protection on the auth surface after nginx was dropped
//! fleet-wide (`limit_req` went with it — the accepted-risk gap in TECH_DEBT).
//! Dependency-free and in-memory: each node protects its own login endpoint,
//! which is exactly the abuse surface (offline credential guessing against a
//! reachable node). No cross-node coordination needed.
//!
//! Fixed-window counter per key: at most `max_per_window` hits per `window`;
//! over the limit returns the seconds until the window resets. Buckets are
//! pruned opportunistically on each call, so the map stays bounded by the
//! number of distinct keys seen within one window.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub struct RateLimiter {
    buckets: Mutex<HashMap<String, (Instant, u32)>>,
    max_per_window: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self { buckets: Mutex::new(HashMap::new()), max_per_window, window }
    }

    /// Record one hit for `key`. `Ok(())` if allowed; `Err(retry_after_secs)`
    /// when the window is exhausted. A `max_per_window` of 0 disables limiting.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        if self.max_per_window == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut buckets = match self.buckets.lock() {
            Ok(g) => g,
            // Poisoned lock: fail OPEN (never lock users out over our own bug).
            Err(_) => return Ok(()),
        };

        // Opportunistic prune of expired windows keeps the map bounded.
        buckets.retain(|_, (start, _)| now.duration_since(*start) < self.window);

        let entry = buckets.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0); // window rolled over
        }
        if entry.1 >= self.max_per_window {
            let retry = self.window.saturating_sub(now.duration_since(entry.0)).as_secs() + 1;
            return Err(retry);
        }
        entry.1 += 1;
        Ok(())
    }
}

/// Shared limiter for the auth/login surface. Window is 60 s; the per-IP cap
/// comes from `RATE_LIMIT_AUTH_PER_MIN` (default 20, `0` = disabled).
pub fn auth_limiter() -> &'static RateLimiter {
    static LIMITER: OnceLock<RateLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| {
        let max = std::env::var("RATE_LIMIT_AUTH_PER_MIN")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(20);
        RateLimiter::new(max, Duration::from_secs(60))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_limit_then_recovers() {
        let rl = RateLimiter::new(3, Duration::from_millis(80));
        assert!(rl.check("1.2.3.4").is_ok());
        assert!(rl.check("1.2.3.4").is_ok());
        assert!(rl.check("1.2.3.4").is_ok());
        assert!(rl.check("1.2.3.4").is_err(), "4th hit blocked");
        // A different key is independent.
        assert!(rl.check("9.9.9.9").is_ok());
        std::thread::sleep(Duration::from_millis(100));
        assert!(rl.check("1.2.3.4").is_ok(), "window reset");
    }

    #[test]
    fn zero_disables() {
        let rl = RateLimiter::new(0, Duration::from_secs(60));
        for _ in 0..1000 {
            assert!(rl.check("x").is_ok());
        }
    }
}
