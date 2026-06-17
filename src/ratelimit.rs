//! Client-side rate limiting.
//!
//! A cost-weighted token bucket that mirrors the server's model: `limit` is both
//! the requests-per-second ceiling and the burst capacity (the bucket holds one
//! second's worth of tokens), and it refills continuously at `limit` tokens per
//! second. Each request reserves its endpoint's cost weight before going out, so
//! heavier calls draw down the budget faster than a flat per-call delay would.
//!
//! The bucket is authoritative-by-server: it syncs down to the live `remaining`
//! reported by [`crate::types::RateLimitStatus`] (via `GET /account/rate-limit`)
//! and to the `X-RateLimit-*` / `Retry-After` headers on a `429`. This keeps the
//! client pacing itself off real server state rather than a guess — which is what
//! actually prevents IP bans.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;

use crate::RateLimit;

/// Cost-weighted token bucket plus 429/`Retry-After` back-off state.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    /// Whether to proactively pace requests. When `false`, `reserve` is a no-op
    /// but the client still honors `429` + `Retry-After` reactively.
    enabled: bool,
    max_retries: u32,
    state: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    /// Unlimited tier (gateway keys): never throttle.
    unlimited: bool,
    /// Burst capacity, in tokens (== requests/sec).
    capacity: f64,
    /// Refill rate, in tokens/sec (== `capacity`).
    refill_per_sec: f64,
    /// Tokens currently available. May go transiently negative when callers
    /// reserve optimistically; refill brings it back up.
    tokens: f64,
    /// Last time `tokens` was refilled.
    updated: Instant,
    /// Server-imposed back-off (from a `429`): hold requests until this instant.
    blocked_until: Option<Instant>,
}

impl RateLimiter {
    pub(crate) fn new(cfg: &RateLimit) -> Self {
        // Floor the rate so it is always a usable divisor for the refill math.
        let rps = cfg.requests_per_second.max(0.001);
        Self {
            enabled: cfg.limiter_enabled,
            max_retries: cfg.max_retries,
            state: Mutex::new(Bucket {
                unlimited: false,
                capacity: rps,
                refill_per_sec: rps,
                tokens: rps,
                updated: Instant::now(),
                blocked_until: None,
            }),
        }
    }

    pub(crate) fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// Reserve `cost` tokens, returning how long the caller must sleep before
    /// sending. Computes the wait under the lock and releases it immediately;
    /// the actual sleep happens in the caller (never holding the lock across an
    /// `.await`). Concurrent callers self-stagger: each sees the prior callers'
    /// deductions, so the returned waits fan out instead of bunching.
    pub(crate) fn reserve(&self, cost: f64) -> Duration {
        // Free endpoints (cost 0, e.g. the rate-limit status poll) never wait.
        if !self.enabled || cost <= 0.0 {
            return Duration::ZERO;
        }
        let mut b = self.state.lock().unwrap();
        if b.unlimited {
            return Duration::ZERO;
        }

        let now = Instant::now();
        let elapsed = now.saturating_duration_since(b.updated).as_secs_f64();
        b.tokens = (b.tokens + elapsed * b.refill_per_sec).min(b.capacity);
        b.updated = now;

        // Respect an active server back-off window first.
        let mut wait = match b.blocked_until {
            Some(t) if t > now => t - now,
            _ => {
                b.blocked_until = None;
                Duration::ZERO
            }
        };

        // Then wait for enough tokens to accrue, if we're short.
        if b.tokens < cost {
            let secs = ((cost - b.tokens) / b.refill_per_sec).max(0.0);
            let token_wait = Duration::from_secs_f64(secs);
            if token_wait > wait {
                wait = token_wait;
            }
        }

        b.tokens -= cost;
        wait
    }

    /// Fold a `429`'s headers into the bucket: open a back-off window for the
    /// `Retry-After` duration and re-sync capacity/remaining to the server.
    pub(crate) fn note_throttle(&self, info: &ThrottleInfo) {
        let mut b = self.state.lock().unwrap();
        let wait = info.wait(Duration::ZERO);
        if !wait.is_zero() {
            let until = Instant::now() + wait;
            b.blocked_until = Some(match b.blocked_until {
                Some(existing) if existing > until => existing,
                _ => until,
            });
        }
        if let Some(limit) = info.limit {
            if limit > 0 {
                b.unlimited = false;
                b.capacity = limit as f64;
                b.refill_per_sec = limit as f64;
            }
        }
        if let Some(remaining) = info.remaining {
            b.tokens = (remaining as f64).min(b.capacity);
            b.updated = Instant::now();
        }
    }

    /// Sync to a [`RateLimitStatus`](crate::types::RateLimitStatus) snapshot
    /// (`GET /account/rate-limit`). `None` limit means the unlimited tier, which
    /// disables throttling; otherwise capacity/refill follow `limit` and the
    /// available tokens are clamped down to the server's `remaining`.
    pub(crate) fn sync(&self, limit: Option<u32>, remaining: Option<u32>) {
        let mut b = self.state.lock().unwrap();
        match limit {
            Some(l) if l > 0 => {
                b.unlimited = false;
                b.capacity = l as f64;
                b.refill_per_sec = l as f64;
            }
            // limit == 0 is nonsensical; leave the current capacity in place.
            Some(_) => {}
            // Null limit => unlimited tier.
            None => b.unlimited = true,
        }
        if let Some(r) = remaining {
            let cap = if b.unlimited { f64::MAX } else { b.capacity };
            b.tokens = (r as f64).min(cap);
            b.updated = Instant::now();
        }
    }
}

/// The rate-limit signal carried on a `429` response's headers.
#[derive(Debug, Default)]
pub(crate) struct ThrottleInfo {
    pub(crate) retry_after: Option<Duration>,
    pub(crate) limit: Option<u32>,
    pub(crate) remaining: Option<u32>,
    /// `X-RateLimit-Reset`: unix timestamp (seconds) when the limit resets.
    pub(crate) reset: Option<i64>,
}

impl ThrottleInfo {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        fn parse<T: std::str::FromStr>(headers: &HeaderMap, name: &str) -> Option<T> {
            headers.get(name)?.to_str().ok()?.trim().parse().ok()
        }
        Self {
            retry_after: parse::<u64>(headers, "retry-after").map(Duration::from_secs),
            limit: parse(headers, "x-ratelimit-limit"),
            remaining: parse(headers, "x-ratelimit-remaining"),
            reset: parse(headers, "x-ratelimit-reset"),
        }
    }

    /// How long to wait before retrying: prefer `Retry-After`, then the time
    /// until `X-RateLimit-Reset`, else the caller's `fallback` back-off.
    pub(crate) fn wait(&self, fallback: Duration) -> Duration {
        if let Some(d) = self.retry_after {
            return d;
        }
        if let Some(reset) = self.reset {
            if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
                let now_secs = now.as_secs() as i64;
                if reset > now_secs {
                    return Duration::from_secs((reset - now_secs) as u64);
                }
            }
        }
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(rps: f64) -> RateLimiter {
        RateLimiter::new(&RateLimit {
            limiter_enabled: true,
            requests_per_second: rps,
            max_retries: 3,
        })
    }

    #[test]
    fn full_bucket_does_not_wait_until_drained() {
        let rl = limiter(2.0); // capacity 2 tokens, refills 2/sec
        assert_eq!(rl.reserve(1.0), Duration::ZERO);
        assert_eq!(rl.reserve(1.0), Duration::ZERO);
        // Bucket now empty: the next token costs ~0.5s, not a flat per-call delay.
        let wait = rl.reserve(1.0);
        assert!(wait > Duration::ZERO);
        assert!(
            wait <= Duration::from_secs_f64(0.6),
            "unexpected wait: {wait:?}"
        );
    }

    #[test]
    fn heavier_cost_weight_waits_longer() {
        let cheap = limiter(4.0);
        cheap.reserve(4.0); // drain
        let cheap_wait = cheap.reserve(1.0);

        let heavy = limiter(4.0);
        heavy.reserve(4.0); // drain
        let heavy_wait = heavy.reserve(3.0);

        assert!(heavy_wait > cheap_wait);
    }

    #[test]
    fn free_endpoints_never_wait() {
        let rl = limiter(1.0);
        rl.reserve(1.0); // drain the single token
        assert_eq!(rl.reserve(0.0), Duration::ZERO);
    }

    #[test]
    fn disabled_limiter_never_waits() {
        let rl = RateLimiter::new(&RateLimit {
            limiter_enabled: false,
            requests_per_second: 1.0,
            max_retries: 3,
        });
        for _ in 0..10 {
            assert_eq!(rl.reserve(1.0), Duration::ZERO);
        }
    }

    #[test]
    fn sync_to_unlimited_disables_throttling() {
        let rl = limiter(1.0);
        rl.reserve(1.0); // drain
        rl.sync(None, None); // unlimited tier
        assert_eq!(rl.reserve(100.0), Duration::ZERO);
    }

    #[test]
    fn sync_clamps_tokens_down_to_server_remaining() {
        let rl = limiter(100.0); // local bucket thinks it has 100 tokens
        rl.sync(Some(100), Some(0)); // server says 0 remaining
        let wait = rl.reserve(1.0);
        assert!(
            wait > Duration::ZERO,
            "should wait when server reports empty"
        );
    }
}
