//! Client configuration.

use crate::auth::Credentials;
use crate::ws::Backoff;
use std::sync::Arc;
use std::time::Duration;

use backon::ExponentialBuilder;

/// Default bound on the WebSocket event channel. Once this many events are
/// buffered ahead of a slow consumer, the read loop stops pulling frames off
/// the socket (backpressure) rather than buffering without limit.
const DEFAULT_WS_CHANNEL_CAPACITY: usize = 1024;

/// Which Nexus Exchange environment to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Network {
    /// Production / stable channel.
    Stable,
    /// Beta channel (tracks `main`; may break).
    Beta,
    /// Local development server.
    Local,
}

impl Network {
    /// Base URL for this network.
    pub fn base_url(self) -> &'static str {
        match self {
            Network::Stable => "https://exchange.nexus.xyz/api/exchange",
            Network::Beta => "https://beta.exchange.nexus.xyz/api/exchange",
            Network::Local => "http://localhost:9090",
        }
    }

    /// WebSocket URL for this network (the `/ws` endpoint).
    pub fn ws_url(self) -> &'static str {
        match self {
            Network::Stable => "wss://exchange.nexus.xyz/api/exchange/ws",
            Network::Beta => "wss://beta.exchange.nexus.xyz/api/exchange/ws",
            Network::Local => "ws://localhost:9090/ws",
        }
    }
}

/// Tunables for the streaming WebSocket client.
#[derive(Debug, Clone)]
pub(crate) struct WsConfig {
    /// Reconnect backoff policy (exponential + jitter).
    pub(crate) backoff: Backoff,
    /// Bound on the buffered-event channel handed to the consumer.
    pub(crate) channel_capacity: usize,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            backoff: Backoff::new(),
            channel_capacity: DEFAULT_WS_CHANNEL_CAPACITY,
        }
    }
}

/// Client-side rate-limit policy.
///
/// The client always honors `429` + `Retry-After` reactively (bounded by
/// [`max_retries`](Self::max_retries)). When [`limiter_enabled`](Self::limiter_enabled)
/// is set, it *also* paces requests proactively through a cost-weighted token
/// bucket so it rarely hits a `429` in the first place.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RateLimit {
    /// Proactively pace requests with the cost-weighted token bucket. When
    /// `false`, only the reactive `429`/`Retry-After` handling applies.
    pub limiter_enabled: bool,
    /// Initial requests-per-second budget (also the burst capacity). Used until
    /// the server reports the caller's real tier via a `429` or
    /// [`Client::fetch_rate_limit_status`](crate::Client::fetch_rate_limit_status).
    pub requests_per_second: f64,
    /// Maximum automatic retries on a `429` before returning
    /// [`Error::RateLimited`](crate::Error::RateLimited).
    pub max_retries: u32,
}

impl Default for RateLimit {
    fn default() -> Self {
        // Conservative until the server tells us the real tier; self-corrects on
        // the first 429 or rate-limit-status sync.
        Self {
            limiter_enabled: true,
            requests_per_second: 10.0,
            max_retries: 3,
        }
    }
}

impl RateLimit {
    /// A policy with the proactive limiter enabled at `requests_per_second` and
    /// the default retry ceiling. Start here and tune with the builder methods.
    ///
    /// `RateLimit` is `#[non_exhaustive]`, so construct it through this
    /// constructor (or [`RateLimit::default`]) rather than a struct literal —
    /// new knobs can then be added without a breaking change.
    pub fn new(requests_per_second: f64) -> Self {
        Self {
            requests_per_second,
            ..Self::default()
        }
    }

    /// Toggle proactive token-bucket pacing. With it off, only the reactive
    /// `429` + `Retry-After` handling applies.
    pub fn with_limiter_enabled(mut self, enabled: bool) -> Self {
        self.limiter_enabled = enabled;
        self
    }

    /// Set the requests-per-second budget (also the burst capacity).
    pub fn with_requests_per_second(mut self, requests_per_second: f64) -> Self {
        self.requests_per_second = requests_per_second;
        self
    }

    /// Set the maximum automatic retries on a `429`.
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }
}

/// How the client retries [transient](crate::Error::is_transient) failures on
/// idempotent (`GET`) requests.
///
/// This layer is distinct from the rate limiter: it covers connect/timeout
/// transport errors and `5xx`/`408` responses. `429` is **not** retried here —
/// that is owned end-to-end by [`RateLimit`] (`Retry-After` + token bucket), so
/// the two don't double-retry the same failure.
///
/// Retries use exponential backoff with jitter: the base delay before retry `n`
/// is `min_delay * factor^n` (capped at `max_delay`), and jitter adds a random
/// amount in `(0, current_delay)` *on top of* that base. Jitter spreads retries
/// out so that many clients failing at once don't synchronize into a thundering
/// herd. Disable it with [`RetryConfig::jitter`] set to `false` (e.g. for
/// deterministic tests).
///
/// **The per-request timeout is per *attempt*, not per call.** A call that
/// retries `max_retries` times can take up to `(max_retries + 1) * timeout`
/// plus backoff before it surfaces an error. Use [`RetryConfig::max_total_delay`]
/// to bound the time spent *sleeping* between attempts (it does not bound the
/// attempts themselves).
///
/// ```
/// use std::time::Duration;
/// use nexus_exchange::RetryConfig;
///
/// let retry = RetryConfig {
///     max_retries: 5,
///     min_delay: Duration::from_millis(50),
///     ..RetryConfig::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retries *after* the initial attempt. `0` disables
    /// retries entirely (one attempt, no backoff).
    pub max_retries: usize,
    /// Base delay used for the first backoff step.
    pub min_delay: Duration,
    /// Upper bound on the *base* backoff delay before jitter. With
    /// [`jitter`](Self::jitter) enabled, a single delay can exceed this by up to
    /// the base again (jitter adds a random `(0, current_delay)` on top), so
    /// this is not a hard per-delay ceiling — only [`max_total_delay`](Self::max_total_delay)
    /// bounds total sleep.
    pub max_delay: Duration,
    /// Multiplier applied to the delay after each attempt. Must be `>= 1.0`;
    /// values below `1.0` (or `NaN`) would shrink the delay each step, so they
    /// are clamped up to `1.0` (constant delay) rather than silently degrading
    /// the backoff.
    pub factor: f32,
    /// Whether to add jitter (a random amount in `(0, current_delay)`) to
    /// backoff delays.
    pub jitter: bool,
    /// Optional cap on the *total* time spent sleeping between attempts. `None`
    /// (the default) means retries are bounded only by `max_retries` and
    /// `max_delay`. Note this bounds inter-attempt backoff, not the time spent
    /// inside the attempts themselves (which the per-request timeout bounds).
    pub max_total_delay: Option<Duration>,
}

impl RetryConfig {
    /// A [`RetryConfig`] that performs no retries.
    pub fn disabled() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Translate into the backoff policy consumed by the retry layer.
    pub(crate) fn backoff(&self) -> ExponentialBuilder {
        // A factor below 1.0 (or NaN) would shrink the delay each step instead
        // of growing it — backon accepts it silently, so clamp to 1.0 (constant
        // delay) here to keep backoff monotonic regardless of caller input.
        let factor = if self.factor >= 1.0 { self.factor } else { 1.0 };
        let builder = ExponentialBuilder::default()
            .with_min_delay(self.min_delay)
            .with_max_delay(self.max_delay)
            .with_factor(factor)
            .with_max_times(self.max_retries)
            .with_total_delay(self.max_total_delay);
        if self.jitter {
            builder.with_jitter()
        } else {
            builder
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            min_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            factor: 2.0,
            jitter: true,
            max_total_delay: None,
        }
    }
}

/// Default per-request timeout. Generous enough for cold connections, tight
/// enough to surface a stalled request rather than hang indefinitely.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Client configuration. Credentials are optional — public market-data
/// endpoints need none.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: String,
    pub(crate) ws_url: String,
    pub(crate) ws: WsConfig,
    pub(crate) rate_limit: RateLimit,
    pub(crate) credentials: Option<Arc<Credentials>>,
    pub(crate) timeout: Duration,
    pub(crate) retry: RetryConfig,
}

impl Config {
    /// Target the given [`Network`], unauthenticated.
    pub fn new(network: Network) -> Self {
        Self {
            base_url: network.base_url().to_string(),
            ws_url: network.ws_url().to_string(),
            ws: WsConfig::default(),
            rate_limit: RateLimit::default(),
            credentials: None,
            timeout: DEFAULT_TIMEOUT,
            retry: RetryConfig::default(),
        }
    }

    /// Target a custom REST base URL (e.g. a preview deployment),
    /// unauthenticated. The WebSocket URL is derived from it (scheme swapped to
    /// `ws(s)` and `/ws` appended); override it explicitly with
    /// [`Config::with_ws_url`].
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let ws_url = derive_ws_url(&base_url);
        Self {
            base_url,
            ws_url,
            ws: WsConfig::default(),
            rate_limit: RateLimit::default(),
            credentials: None,
            timeout: DEFAULT_TIMEOUT,
            retry: RetryConfig::default(),
        }
    }

    /// Set the per-request timeout. This bounds each individual attempt; a
    /// timed-out attempt is [transient](crate::Error::is_transient) and so is
    /// subject to retry on idempotent (`GET`) requests. Because it is
    /// per-attempt, a retried call can take a multiple of this value — see
    /// [`RetryConfig`] for the total-time bound.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configure how transient failures on idempotent requests are retried.
    /// Pass [`RetryConfig::disabled`] to turn this layer off (the `429`
    /// rate-limit handling is independent — see [`Config::with_rate_limit`]).
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Override the WebSocket URL.
    pub fn with_ws_url(mut self, ws_url: impl Into<String>) -> Self {
        self.ws_url = ws_url.into();
        self
    }

    /// Override the reconnect backoff policy used by the streaming client.
    pub fn with_reconnect_backoff(mut self, backoff: Backoff) -> Self {
        self.ws.backoff = backoff;
        self
    }

    /// Set the capacity of the WebSocket event channel. A smaller bound makes
    /// backpressure kick in sooner; a larger one tolerates burstier consumers.
    /// Clamped to at least `1`.
    pub fn with_channel_capacity(mut self, capacity: usize) -> Self {
        self.ws.channel_capacity = capacity.max(1);
        self
    }

    /// Override the rate-limit policy.
    pub fn with_rate_limit(mut self, rate_limit: RateLimit) -> Self {
        self.rate_limit = rate_limit;
        self
    }

    /// Disable proactive client-side pacing. `429` + `Retry-After` is still
    /// honored reactively.
    pub fn without_rate_limiter(mut self) -> Self {
        self.rate_limit.limiter_enabled = false;
        self
    }

    /// Authenticate with an HMAC API key — `key_id` and the 64-char hex
    /// `secret` from `POST /keys`.
    pub fn api_key(mut self, key_id: impl Into<String>, secret: impl Into<String>) -> Self {
        self.credentials = Some(Arc::new(Credentials::api_key(key_id, secret)));
        self
    }

    /// Authenticate with a session bearer token from `POST /auth/login`.
    pub fn session_token(mut self, token: impl Into<String>) -> Self {
        self.credentials = Some(Arc::new(Credentials::session(token)));
        self
    }

    /// The configured REST base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The configured WebSocket URL.
    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }
}

/// Derive a WebSocket URL from a REST base URL: swap the scheme to `ws`/`wss`
/// and append the `/ws` endpoint. Unknown schemes are left as-is.
fn derive_ws_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let swapped = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{swapped}/ws")
}

impl Default for Config {
    fn default() -> Self {
        Self::new(Network::Stable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backon::BackoffBuilder;

    /// A `factor < 1.0` (or NaN) must not produce a shrinking backoff — it is
    /// clamped to a constant delay rather than degrading silently.
    #[test]
    fn degenerate_factor_is_clamped_to_non_shrinking_delay() {
        for factor in [0.5_f32, f32::NAN] {
            let cfg = RetryConfig {
                factor,
                jitter: false,
                min_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(5),
                max_retries: 3,
                max_total_delay: None,
            };
            let mut delays = cfg.backoff().build();
            let first = delays.next().expect("at least one delay");
            let second = delays.next().expect("at least two delays");
            assert!(
                second >= first,
                "factor {factor} produced a shrinking delay: {second:?} < {first:?}",
            );
        }
    }

    #[test]
    fn derives_ws_url_from_https_base() {
        assert_eq!(
            derive_ws_url("https://exchange.nexus.xyz/api/exchange"),
            "wss://exchange.nexus.xyz/api/exchange/ws"
        );
    }

    #[test]
    fn derives_ws_url_from_http_base_with_trailing_slash() {
        assert_eq!(
            derive_ws_url("http://localhost:9090/"),
            "ws://localhost:9090/ws"
        );
    }

    #[test]
    fn channel_capacity_is_clamped_to_at_least_one() {
        let cfg = Config::default().with_channel_capacity(0);
        assert_eq!(cfg.ws.channel_capacity, 1);
    }
}
