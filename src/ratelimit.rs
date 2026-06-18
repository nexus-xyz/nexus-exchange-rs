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

/// Upper bound on any server-supplied back-off (`Retry-After` or
/// `X-RateLimit-Reset`). A hostile or buggy gateway could otherwise send a
/// `Retry-After` of years and wedge the client — or, via `note_throttle`, the
/// whole shared limiter — indefinitely. We honor the server's hint up to this
/// ceiling and no further.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

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
            // Infallible: `try_from_secs_f64` rejects NaN/negative/overflow, so a
            // pathological (tiny refill, huge cost) pair saturates instead of
            // panicking in the request hot path.
            let token_wait = Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX);
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
            retry_after: parse_retry_after(headers),
            limit: parse(headers, "x-ratelimit-limit"),
            remaining: parse(headers, "x-ratelimit-remaining"),
            reset: parse(headers, "x-ratelimit-reset"),
        }
    }

    /// How long to wait before retrying: prefer `Retry-After`, then the time
    /// until `X-RateLimit-Reset`, else the caller's `fallback` back-off. Both
    /// server-supplied hints are clamped to [`MAX_RETRY_AFTER`] so a bogus far-
    /// future `reset` can't wedge the client any more than a bogus `Retry-After`.
    pub(crate) fn wait(&self, fallback: Duration) -> Duration {
        if let Some(d) = self.retry_after {
            return d.min(MAX_RETRY_AFTER);
        }
        if let Some(reset) = self.reset {
            if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
                let now_secs = now.as_secs() as i64;
                if reset > now_secs {
                    return Duration::from_secs((reset - now_secs) as u64).min(MAX_RETRY_AFTER);
                }
            }
        }
        fallback
    }
}

/// Parse the `Retry-After` header, honoring both RFC 9110 forms — `delta-seconds`
/// (`Retry-After: 120`) and `HTTP-date` (`Retry-After: Wed, 21 Oct 2026 07:28:00
/// GMT`) — and clamping the result to [`MAX_RETRY_AFTER`]. The date form is
/// converted to a delay from now; a date in the past yields `Duration::ZERO`.
fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let raw = headers.get("retry-after")?.to_str().ok()?;
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER));
    }
    let when = httpdate::parse_http_date(raw).ok()?;
    let delay = when
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    Some(delay.min(MAX_RETRY_AFTER))
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

    fn headers(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderName, HeaderValue};
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn retry_after_delta_seconds_is_parsed() {
        let info = ThrottleInfo::from_headers(&headers(&[("retry-after", "12")]));
        assert_eq!(info.retry_after, Some(Duration::from_secs(12)));
    }

    #[test]
    fn retry_after_is_clamped_to_max() {
        // A hostile gateway asks the client to sleep for ~3170 years.
        let info = ThrottleInfo::from_headers(&headers(&[("retry-after", "99999999999")]));
        assert_eq!(info.retry_after, Some(MAX_RETRY_AFTER));
        // And the clamp survives through `wait`, the value the client sleeps on.
        assert_eq!(info.wait(Duration::ZERO), MAX_RETRY_AFTER);
    }

    #[test]
    fn retry_after_http_date_form_is_honored() {
        // RFC 9110 HTTP-date form, ~60s in the future.
        let when = SystemTime::now() + Duration::from_secs(60);
        let date = httpdate::fmt_http_date(when);
        let info = ThrottleInfo::from_headers(&headers(&[("retry-after", &date)]));
        let d = info.retry_after.expect("date form should parse to a delay");
        assert!(
            d > Duration::from_secs(50) && d <= Duration::from_secs(61),
            "unexpected delay from HTTP-date: {d:?}"
        );
    }

    #[test]
    fn retry_after_http_date_in_past_is_zero() {
        let when = SystemTime::now() - Duration::from_secs(60);
        let date = httpdate::fmt_http_date(when);
        let info = ThrottleInfo::from_headers(&headers(&[("retry-after", &date)]));
        assert_eq!(info.retry_after, Some(Duration::ZERO));
    }

    #[test]
    fn wait_falls_back_to_reset_when_no_retry_after() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let info = ThrottleInfo {
            reset: Some(now + 30),
            ..Default::default()
        };
        let d = info.wait(Duration::from_secs(1));
        assert!(
            d > Duration::from_secs(25) && d <= Duration::from_secs(31),
            "expected ~30s from reset, got {d:?}"
        );

        // A bogus far-future reset is clamped, not honored verbatim.
        let bogus = ThrottleInfo {
            reset: Some(now + 1_000_000),
            ..Default::default()
        };
        assert_eq!(bogus.wait(Duration::from_secs(1)), MAX_RETRY_AFTER);

        // A reset in the past leaves the caller's fallback in place.
        let past = ThrottleInfo {
            reset: Some(now - 30),
            ..Default::default()
        };
        assert_eq!(past.wait(Duration::from_secs(7)), Duration::from_secs(7));
    }

    #[test]
    fn note_throttle_retunes_capacity_from_429_headers() {
        let rl = limiter(2.0); // capacity 2 tokens
                               // 429 reveals the real tier: 100 req/s, currently full.
        rl.note_throttle(&ThrottleInfo {
            limit: Some(100),
            remaining: Some(100),
            ..Default::default()
        });
        // With the old capacity of 2 this would wait; the re-tune to 100 lets it
        // through immediately, proving the bucket self-corrected off the headers.
        assert_eq!(rl.reserve(50.0), Duration::ZERO);
    }
}
