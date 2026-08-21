//! Per-layer event rate limiting.
//!
//! Uses a token-bucket filter that drops events when the calling thread
//! produces log events faster than the configured rate. Useful for
//! preventing tight-loop error floods from overwhelming log storage.

use std::sync::Mutex;
use std::time::Instant;

use tracing::Metadata;

/// Rate-limit configuration for a layer.
///
/// The type is `#[non_exhaustive]`, so struct-literal syntax is not available
/// outside this crate. Use one of the constructors:
///
/// ```rust
/// use stratify::logging::rate_limit::RateLimit;
///
/// let per_second = RateLimit::per_second(100);
/// let per_minute = RateLimit::per_minute(1_000);
/// let custom = RateLimit::new(50, 15); // 50 events per 15 seconds
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RateLimit {
    /// Maximum events allowed per `per_secs` seconds.
    pub max_events: u64,
    /// Window size in seconds (e.g. 1 = per-second, 60 = per-minute).
    pub per_secs: u64,
}

impl RateLimit {
    /// Allow at most `max_events` events per `per_secs` seconds.
    ///
    /// The two fields only make sense together, so they are set together
    /// rather than through separate chainable setters.
    pub fn new(max_events: u64, per_secs: u64) -> Self {
        Self {
            max_events,
            per_secs,
        }
    }

    /// Allow at most `max_events` events per second.
    pub fn per_second(max_events: u64) -> Self {
        Self::new(max_events, 1)
    }

    /// Allow at most `max_events` events per minute.
    pub fn per_minute(max_events: u64) -> Self {
        Self::new(max_events, 60)
    }
}

/// A token-bucket rate limiter that can be used as a `tracing_subscriber` filter
/// or passed to `Layer::filter_fn`.
#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimit,
    state: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter with the given config.
    pub fn new(config: RateLimit) -> Self {
        Self {
            config,
            state: Mutex::new(Bucket {
                tokens: config.max_events as f64,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Consume a token. Returns `true` if the event should be allowed through.
    ///
    /// `RateLimiter` implements [`EventGate`](super::gate::EventGate) in terms
    /// of this method, which is how
    /// [`Builder::rate_limit`](super::Builder::rate_limit) applies it. Compose
    /// it by hand the same way — *not* as a
    /// [`FilterFn`](tracing_subscriber::filter::FilterFn), which caches its
    /// first verdict per callsite and would leave the bucket frozen after one
    /// event:
    ///
    /// ```rust
    /// use stratify::logging::gate::{EventGate, GateLayer};
    /// use stratify::logging::rate_limit::{RateLimit, RateLimiter};
    /// use tracing_subscriber::layer::SubscriberExt;
    /// use tracing_subscriber::Registry;
    ///
    /// let limiter = RateLimiter::new(RateLimit::per_second(5));
    /// let gates: Vec<Box<dyn EventGate>> = vec![Box::new(limiter)];
    /// let subscriber = Registry::default().with(GateLayer::new(gates));
    ///
    /// tracing::subscriber::with_default(subscriber, || {
    ///     for _ in 0..100 {
    ///         tracing::info!("only the first five get through");
    ///     }
    /// });
    /// ```
    pub fn allow(&self) -> bool {
        let mut bucket = match self.state.lock() {
            Ok(b) => b,
            Err(_) => return false, // poisoned mutex — drop gracefully
        };

        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();

        if elapsed > 0.0 {
            let refill_rate = self.config.max_events as f64 / self.config.per_secs as f64;
            bucket.tokens =
                (bucket.tokens + elapsed * refill_rate).min(self.config.max_events as f64);
            bucket.last_refill = now;
        }

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl super::gate::EventGate for RateLimiter {
    /// Rate limiting is volume control, so the verdict ignores `meta` — every
    /// event costs the same single token regardless of level or target.
    fn allows(&self, _meta: &Metadata<'_>) -> bool {
        self.allow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_second_defaults() {
        let rl = RateLimit::per_second(10);
        assert_eq!(rl.max_events, 10);
        assert_eq!(rl.per_secs, 1);
    }

    #[test]
    fn per_minute_defaults() {
        let rl = RateLimit::per_minute(60);
        assert_eq!(rl.max_events, 60);
        assert_eq!(rl.per_secs, 60);
    }

    #[test]
    fn allows_events_up_to_limit() {
        let limiter = RateLimiter::new(RateLimit::per_second(5));
        for _ in 0..5 {
            assert!(limiter.allow(), "first 5 events should be allowed");
        }
    }

    #[test]
    fn throttles_after_limit() {
        let limiter = RateLimiter::new(RateLimit::per_second(3));
        for _ in 0..3 {
            assert!(limiter.allow());
        }
        // Next call should be throttled (no time has elapsed)
        assert!(!limiter.allow(), "4th event should be throttled");
    }

    #[test]
    fn refills_over_time() {
        let limiter = RateLimiter::new(RateLimit::per_second(100));
        // Consume all tokens
        for _ in 0..100 {
            assert!(limiter.allow());
        }
        assert!(!limiter.allow());

        // Sleep for enough time to refill 10%
        std::thread::sleep(std::time::Duration::from_millis(150));
        for _ in 0..10 {
            assert!(limiter.allow(), "should refill ~10 tokens after 150ms");
        }
    }

    #[test]
    fn token_cap_respected() {
        let limiter = RateLimiter::new(RateLimit::per_second(5));
        // Sleep enough for 10 tokens to accumulate, but cap should hold at 5
        std::thread::sleep(std::time::Duration::from_secs(2));
        let mut allowed = 0;
        while limiter.allow() {
            allowed += 1;
        }
        assert_eq!(allowed, 5, "token bucket should cap at max_events");
    }
}
