//! Event sampling — probabilistic and per-level filtering.
//!
//! Sampling gates events before they reach any format/write layer,
//! reducing log volume at the source. Complements rate limiting by
//! offering a different axis of control: instead of limiting throughput,
//! it drops a configurable fraction of events statistically.
//!
//! # How it works
//!
//! A thread-local linear congruential generator provides pseudo-randomness
//! without an external `rand` dependency, seeded per thread so that threads do
//! not correlate their decisions. Events more verbose than `min_level` are
//! always dropped; the rest pass with probability `rate`.
//!
//! Most callers want [`Builder::sampling`](super::Builder::sampling), which
//! wires this up for them:
//!
//! ```rust
//! use stratify::logging::sampling::SampleConfig;
//! use stratify::logging::{ConsoleConfig};
//! use tracing::level_filters::LevelFilter;
//!
//! # fn main() -> Result<(), stratify::logging::Error> {
//! let (subscriber, _handle) = stratify::logging::builder()
//!     .console(ConsoleConfig::default())
//!     .sampling(SampleConfig::new(0.1).with_min_level(LevelFilter::DEBUG))
//!     .build()?;
//! # let _ = subscriber;
//! # Ok(())
//! # }
//! ```
//!
//! To compose a sampler by hand, use it as an
//! [`EventGate`](super::gate::EventGate) — not as a
//! [`FilterFn`](tracing_subscriber::filter::FilterFn), which caches its first
//! verdict per callsite and would freeze the sampler after one event:
//!
//! ```rust
//! use stratify::logging::gate::{EventGate, GateLayer};
//! use stratify::logging::sampling::{SampleConfig, Sampler};
//! use tracing_subscriber::layer::SubscriberExt;
//! use tracing_subscriber::Registry;
//!
//! let sampler = Sampler::new(SampleConfig::new(0.1));
//! let gates: Vec<Box<dyn EventGate>> = vec![Box::new(sampler)];
//!
//! let subscriber = Registry::default().with(GateLayer::new(gates));
//! let _guard = tracing::subscriber::set_default(subscriber);
//! ```

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::level_filters::LevelFilter;
use tracing::Metadata;

// Thread-local LCG state. Seeded per thread — see `seed_for_new_thread`.
thread_local! {
    static LCG_STATE: RefCell<u64> = RefCell::new(seed_for_new_thread());
}

/// Configuration for event sampling.
///
/// The type is `#[non_exhaustive]`, so struct-literal syntax is not available
/// outside this crate. Use [`SampleConfig::new`] or [`Default`] and chain the
/// `with_*` setters:
///
/// ```rust
/// use stratify::logging::sampling::SampleConfig;
/// use tracing::level_filters::LevelFilter;
///
/// let config = SampleConfig::new(0.1).with_min_level(LevelFilter::DEBUG);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct SampleConfig {
    /// Probability [0.0, 1.0] that an event at or above `min_level` passes.
    /// 1.0 = log everything, 0.0 = log nothing, 0.01 = log 1%.
    pub rate: f64,
    /// The least severe level allowed through. Events *more verbose* than this
    /// are always dropped, regardless of `rate` — with `min_level = DEBUG`,
    /// `ERROR`/`WARN`/`INFO`/`DEBUG` are eligible and `TRACE` is dropped.
    ///
    /// Note that `tracing::Level` orders by verbosity, not severity
    /// (`Level::ERROR < Level::DEBUG` is `true`), so the eligibility test is
    /// `level <= min_level`.
    pub min_level: LevelFilter,
}

impl Default for SampleConfig {
    fn default() -> Self {
        Self {
            rate: 1.0,
            min_level: LevelFilter::TRACE,
        }
    }
}

impl SampleConfig {
    /// Sample at `rate`, admitting every level.
    ///
    /// `rate` is clamped to [0.0, 1.0]: it is a probability, and letting an
    /// out-of-range value through would make `should_sample` behave
    /// unpredictably rather than fail visibly.
    pub fn new(rate: f64) -> Self {
        Self::default().with_rate(rate)
    }

    /// Probability that an eligible event passes. Clamped to [0.0, 1.0].
    pub fn with_rate(mut self, rate: f64) -> Self {
        self.rate = rate.clamp(0.0, 1.0);
        self
    }

    /// The least severe level eligible for sampling; anything more verbose is
    /// always dropped.
    pub fn with_min_level(mut self, min_level: LevelFilter) -> Self {
        self.min_level = min_level;
        self
    }
}

/// Event sampler with thread-local pseudo-randomness.
#[derive(Debug, Clone)]
pub struct Sampler {
    config: SampleConfig,
}

impl Sampler {
    /// Create a new sampler with the given configuration.
    pub fn new(config: SampleConfig) -> Self {
        Self { config }
    }

    /// Returns `true` if the event should be logged.
    ///
    /// Consulted once per event, so it must stay cheap. `Sampler` implements
    /// [`EventGate`](super::gate::EventGate) in terms of this method, which is
    /// how [`Builder::sampling`](super::Builder::sampling) applies it.
    ///
    /// ```rust
    /// use stratify::logging::gate::{EventGate, GateLayer};
    /// use stratify::logging::sampling::{SampleConfig, Sampler};
    /// use tracing::level_filters::LevelFilter;
    /// use tracing_subscriber::layer::SubscriberExt;
    /// use tracing_subscriber::Registry;
    ///
    /// let sampler = Sampler::new(SampleConfig::new(1.0).with_min_level(LevelFilter::INFO));
    /// let gates: Vec<Box<dyn EventGate>> = vec![Box::new(sampler)];
    /// let subscriber = Registry::default().with(GateLayer::new(gates));
    ///
    /// tracing::subscriber::with_default(subscriber, || {
    ///     tracing::info!("should_sample returns true for this");
    ///     tracing::debug!("dropped — more verbose than min_level");
    /// });
    /// ```
    pub fn should_sample(&self, meta: &Metadata) -> bool {
        // Level filter: drop anything more verbose than min_level.
        // `Level` orders by verbosity (ERROR < WARN < INFO < DEBUG < TRACE),
        // so "more verbose than min_level" is `>`, not `<`.
        let level = meta.level();
        if *level > self.config.min_level {
            return false;
        }

        // Probabilistic gate: deterministic pseudo-random per thread.
        if self.config.rate >= 1.0 {
            return true;
        }
        if self.config.rate <= 0.0 {
            return false;
        }
        thread_random() < self.config.rate
    }
}

impl super::gate::EventGate for Sampler {
    fn allows(&self, meta: &Metadata<'_>) -> bool {
        self.should_sample(meta)
    }
}

/// Thread-local linear congruential generator.
///
/// Recurrence: `s ← s * 6364136223846793005 + 1442695040888963407 (mod 2^64)`
/// — the parameters Knuth specifies for MMIX. The modulus is the natural
/// wrap-around of `u64`; because a power-of-two-modulus LCG has very short
/// cycles in its low bits, the sample is taken from the top 32 bits.
///
/// Returns a value in [0.0, 1.0).
fn thread_random() -> f64 {
    /// MMIX multiplier: ≡ 1 (mod 4), which with an odd increment gives the
    /// generator its full 2^64 period.
    const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
    /// MMIX increment (odd, as full period requires).
    const INCREMENT: u64 = 1_442_695_040_888_963_407;
    /// 2^32 — one past the largest value the top 32 bits can hold.
    const SCALE: f64 = 4_294_967_296.0;

    LCG_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        *state = state.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT);
        f64::from((*state >> 32) as u32) / SCALE
    })
}

/// Monotonic counter handing each thread a distinct ordinal.
static THREAD_ORDINAL: AtomicU64 = AtomicU64::new(0);

/// Derive a starting state for a thread that has not drawn a number yet.
///
/// Threads must not share a seed. A shared seed makes every thread draw the
/// same sequence, so sampling decisions correlate perfectly across threads —
/// a 1% sample would keep the *same* 1% of events on each one instead of
/// spreading independently. The seed therefore mixes a per-process nonce (so
/// separate runs differ) with a per-thread ordinal (so threads differ).
fn seed_for_new_thread() -> u64 {
    let ordinal = THREAD_ORDINAL.fetch_add(1, Ordering::Relaxed);
    splitmix64(process_nonce() ^ ordinal.wrapping_mul(GOLDEN_GAMMA))
}

/// A nonce fixed for the lifetime of the process, so two runs of the same
/// program do not replay an identical sampling pattern.
fn process_nonce() -> u64 {
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| {
        let wall_clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(GOLDEN_GAMMA);
        // Mix in a live stack address so two processes that start within the
        // same clock tick still diverge under ASLR.
        let stack_marker = &wall_clock as *const u64 as u64;
        splitmix64(wall_clock ^ stack_marker)
    })
}

/// 2^64 / φ — an odd constant with well-spread bits, used as the stride
/// between consecutive thread seeds.
const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// `splitmix64` finaliser: avalanches inputs that differ only in a few bits,
/// so consecutive thread ordinals become far-apart seeds.
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_logs_everything() {
        let cfg = SampleConfig::default();
        assert_eq!(cfg.rate, 1.0);
        assert_eq!(cfg.min_level, LevelFilter::TRACE);
    }

    // `should_sample` needs real `Metadata`, which only a live subscriber can
    // produce; those cases are covered by `tests/stream_a_sampling.rs`.

    #[test]
    fn thread_random_in_range() {
        for _ in 0..1000 {
            let r = thread_random();
            assert!((0.0..1.0).contains(&r), "got {r}");
        }
    }

    #[test]
    fn thread_random_approximates_uniform() {
        let mut buckets = [0u32; 10];
        for _ in 0..10_000 {
            let r = thread_random();
            let bucket = (r * 10.0) as usize;
            buckets[bucket.min(9)] += 1;
        }
        // Each bucket should have roughly 1000 samples (±30% is generous for LCG)
        for &count in &buckets {
            assert!(count > 700, "bucket underflow: {count}");
            assert!(count < 1300, "bucket overflow: {count}");
        }
    }

    #[test]
    fn sampler_clone_is_independent() {
        let s1 = Sampler::new(SampleConfig::default());
        let _s2 = s1.clone();
        // Config should match
        assert_eq!(s1.config.rate, 1.0);
    }

    /// Draw a fixed-length sequence on the calling thread.
    fn draw_sequence() -> Vec<u64> {
        (0..32).map(|_| thread_random().to_bits()).collect()
    }

    #[test]
    fn separate_threads_draw_different_sequences() {
        // Arrange
        let first = std::thread::spawn(draw_sequence);
        let second = std::thread::spawn(draw_sequence);

        // Act
        let first = first.join().expect("first sampling thread panicked");
        let second = second.join().expect("second sampling thread panicked");

        // Assert
        assert_ne!(
            first, second,
            "threads sharing an LCG seed correlate their sampling decisions"
        );
    }

    #[test]
    fn every_thread_seed_is_distinct() {
        // Arrange
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(seed_for_new_thread))
            .collect();

        // Act
        let mut seeds: Vec<u64> = handles
            .into_iter()
            .map(|h| h.join().expect("seeding thread panicked"))
            .collect();

        // Assert
        let drawn = seeds.len();
        seeds.sort_unstable();
        seeds.dedup();
        assert_eq!(seeds.len(), drawn, "thread seeds collided");
    }

    #[test]
    fn splitmix64_avalanches_adjacent_inputs() {
        // Arrange
        let (low, high) = (splitmix64(0), splitmix64(1));

        // Act
        let differing_bits = (low ^ high).count_ones();

        // Assert — a good finaliser flips roughly half of the 64 bits.
        assert!(
            (16..=48).contains(&differing_bits),
            "adjacent seeds differ in only {differing_bits} bits"
        );
    }
}
