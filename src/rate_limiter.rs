/// Token-bucket rate limiter — one bucket per client IP.
///
/// Capacity  : max burst before throttling kicks in
/// Refill    : tokens added per second (continuous, not tick-based)
///
/// Thread-safe: DashMap gives per-bucket locking — no global mutex.
use std::net::IpAddr;
use std::time::Instant;

use dashmap::DashMap;

struct Bucket {
    tokens: f64,
    last_check: Instant,
}

pub struct RateLimiter {
    buckets: DashMap<IpAddr, Bucket>,
    capacity: f64,
    refill_per_sec: f64,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        RateLimiter {
            buckets: DashMap::new(),
            capacity: capacity as f64,
            refill_per_sec,
        }
    }

    /// Returns true (allow) or false (rate-limited).
    /// Called once per HTTP request — not once per TCP connection.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();

        let mut bucket = self.buckets.entry(ip).or_insert_with(|| Bucket {
            tokens: self.capacity,   // new IPs start with a full bucket
            last_check: now,
        });

        // Refill proportionally to elapsed real time.
        let secs = now.duration_since(bucket.last_check).as_secs_f64();
        bucket.tokens = (bucket.tokens + secs * self.refill_per_sec).min(self.capacity);
        bucket.last_check = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Current token count for an IP (useful for debug/stats endpoints).
    #[allow(dead_code)]
    pub fn tokens(&self, ip: IpAddr) -> f64 {
        self.buckets.get(&ip).map(|b| b.tokens).unwrap_or(self.capacity)
    }
}