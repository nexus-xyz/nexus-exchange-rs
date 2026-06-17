//! Reconnect backoff policy: exponential growth with full jitter.
//!
//! A fixed reconnect sleep is the classic WebSocket failure mode — when an
//! upstream blips, every client wakes on the same cadence and stampedes the
//! endpoint the instant it recovers. This module replaces that with capped
//! exponential backoff plus *full jitter*: each delay is a uniform random draw
//! from `[0, exponential_ceiling)`, which both spaces out a single client's
//! retries and decorrelates a fleet of them. See AWS's "Exponential Backoff
//! And Jitter" for the rationale behind the full-jitter variant.

use std::time::Duration;

/// Default first-retry delay.
const DEFAULT_INITIAL: Duration = Duration::from_millis(500);
/// Default ceiling for a single delay.
const DEFAULT_MAX: Duration = Duration::from_secs(30);
/// Default growth factor between consecutive ceilings.
const DEFAULT_MULTIPLIER: f64 = 2.0;

/// Policy describing how reconnect delays grow after repeated failures.
///
/// A [`Backoff`] is the immutable configuration; call [`Backoff::iter`] to get
/// a [`BackoffIter`] that produces the actual (jittered) delays and can be
/// [`reset`](BackoffIter::reset) once a connection succeeds.
///
/// ```
/// use std::time::Duration;
/// use nexus_exchange::ws::Backoff;
///
/// let policy = Backoff::new()
///     .with_initial(Duration::from_millis(250))
///     .with_max(Duration::from_secs(10));
/// let mut delays = policy.iter();
/// // Every delay stays within the current exponential ceiling.
/// assert!(delays.next_delay() <= Duration::from_millis(250));
/// ```
#[derive(Debug, Clone)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    multiplier: f64,
    jitter: bool,
}

impl Backoff {
    /// A backoff with sensible defaults (500 ms initial, 30 s cap, ×2 growth,
    /// full jitter enabled).
    pub fn new() -> Self {
        Self {
            initial: DEFAULT_INITIAL,
            max: DEFAULT_MAX,
            multiplier: DEFAULT_MULTIPLIER,
            jitter: true,
        }
    }

    /// Set the delay ceiling used for the first retry.
    pub fn with_initial(mut self, initial: Duration) -> Self {
        self.initial = initial;
        self
    }

    /// Set the maximum delay any single retry may wait.
    pub fn with_max(mut self, max: Duration) -> Self {
        self.max = max;
        self
    }

    /// Set the growth factor applied to the ceiling after each retry. Values
    /// below `1.0` are clamped to `1.0` (no growth) to keep delays monotonic.
    pub fn with_multiplier(mut self, multiplier: f64) -> Self {
        self.multiplier = multiplier.max(1.0);
        self
    }

    /// Enable or disable jitter. With jitter off, delays follow the exact
    /// exponential ceiling — useful for deterministic tests, not for fleets.
    pub fn with_jitter(mut self, jitter: bool) -> Self {
        self.jitter = jitter;
        self
    }

    /// Begin a sequence of delays. The returned iterator is independent of the
    /// policy and seeds its own jitter source.
    pub fn iter(&self) -> BackoffIter {
        BackoffIter {
            policy: self.clone(),
            ceiling: self.initial,
            rng: Rng::seeded(),
        }
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// A live sequence of reconnect delays produced from a [`Backoff`] policy.
///
/// Created via [`Backoff::iter`]. Call [`next_delay`](Self::next_delay) before
/// each reconnect attempt and [`reset`](Self::reset) once a connection is
/// established so the next outage starts from the initial delay again.
#[derive(Debug)]
pub struct BackoffIter {
    policy: Backoff,
    /// The current (un-jittered) exponential ceiling for the *next* delay.
    ceiling: Duration,
    rng: Rng,
}

impl BackoffIter {
    /// Produce the next delay and advance the exponential ceiling.
    ///
    /// With jitter enabled the result is uniform in `[0, ceiling]`; with jitter
    /// disabled it is exactly `ceiling`. Either way the ceiling never exceeds
    /// the policy's configured maximum.
    pub fn next_delay(&mut self) -> Duration {
        let ceiling = self.ceiling.min(self.policy.max);
        let delay = if self.policy.jitter {
            ceiling.mul_f64(self.rng.next_unit())
        } else {
            ceiling
        };

        // Grow the ceiling for next time, saturating at the configured max.
        let grown = ceiling.mul_f64(self.policy.multiplier);
        self.ceiling = grown.min(self.policy.max);

        delay
    }

    /// Reset the sequence back to the initial ceiling. Call this after a
    /// successful connection so transient future outages retry promptly.
    pub fn reset(&mut self) {
        self.ceiling = self.policy.initial;
    }
}

/// A tiny SplitMix64 generator. Good enough to scatter reconnect delays; not
/// cryptographic. Kept in-crate so jitter costs no extra dependency.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    /// Seed from the standard library's randomly-keyed hasher so independent
    /// clients (and successive test runs) diverge without pulling in `rand`.
    fn seeded() -> Self {
        use std::hash::{BuildHasher, Hasher};
        let seed = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        // Avoid the all-zero state degenerating the first few outputs.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A float uniformly distributed in `[0, 1)` using 53 bits of entropy.
    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_jitter_follows_exact_exponential_curve() {
        let policy = Backoff::new()
            .with_initial(Duration::from_millis(100))
            .with_max(Duration::from_secs(10))
            .with_multiplier(2.0)
            .with_jitter(false);
        let mut it = policy.iter();
        assert_eq!(it.next_delay(), Duration::from_millis(100));
        assert_eq!(it.next_delay(), Duration::from_millis(200));
        assert_eq!(it.next_delay(), Duration::from_millis(400));
        assert_eq!(it.next_delay(), Duration::from_millis(800));
    }

    #[test]
    fn ceiling_saturates_at_max() {
        let policy = Backoff::new()
            .with_initial(Duration::from_secs(1))
            .with_max(Duration::from_secs(4))
            .with_multiplier(10.0)
            .with_jitter(false);
        let mut it = policy.iter();
        assert_eq!(it.next_delay(), Duration::from_secs(1));
        // Would jump to 10s but is capped at 4s, and stays there.
        assert_eq!(it.next_delay(), Duration::from_secs(4));
        assert_eq!(it.next_delay(), Duration::from_secs(4));
    }

    #[test]
    fn jittered_delays_stay_within_growing_ceiling() {
        let policy = Backoff::new()
            .with_initial(Duration::from_millis(100))
            .with_max(Duration::from_secs(60))
            .with_multiplier(2.0);
        let mut it = policy.iter();
        let mut ceiling = Duration::from_millis(100);
        for _ in 0..16 {
            let d = it.next_delay();
            assert!(d <= ceiling, "delay {d:?} exceeded ceiling {ceiling:?}");
            ceiling = ceiling.mul_f64(2.0).min(Duration::from_secs(60));
        }
    }

    #[test]
    fn reset_returns_to_initial_ceiling() {
        let policy = Backoff::new()
            .with_initial(Duration::from_millis(100))
            .with_multiplier(2.0)
            .with_jitter(false);
        let mut it = policy.iter();
        assert_eq!(it.next_delay(), Duration::from_millis(100));
        assert_eq!(it.next_delay(), Duration::from_millis(200));
        it.reset();
        assert_eq!(it.next_delay(), Duration::from_millis(100));
    }

    #[test]
    fn multiplier_below_one_is_clamped_to_no_growth() {
        let policy = Backoff::new()
            .with_initial(Duration::from_millis(500))
            .with_multiplier(0.1)
            .with_jitter(false);
        let mut it = policy.iter();
        assert_eq!(it.next_delay(), Duration::from_millis(500));
        assert_eq!(it.next_delay(), Duration::from_millis(500));
    }

    #[test]
    fn jitter_actually_varies_delays() {
        let policy = Backoff::new()
            .with_initial(Duration::from_secs(10))
            .with_max(Duration::from_secs(10));
        let mut it = policy.iter();
        // With a 10s ceiling, two consecutive full-jitter draws being bit-for-bit
        // identical is astronomically unlikely; this guards against jitter being
        // silently dropped.
        let a = it.next_delay();
        let b = it.next_delay();
        assert_ne!(a, b);
    }

    #[test]
    fn unit_samples_stay_in_range() {
        let mut rng = Rng(1);
        for _ in 0..10_000 {
            let u = rng.next_unit();
            assert!((0.0..1.0).contains(&u));
        }
    }
}
