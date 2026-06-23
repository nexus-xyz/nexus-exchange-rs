//! Client configuration.

use crate::auth::{Credential, Credentials, Nonce, SystemTimeNonce};
use crate::ws::Backoff;
use std::sync::Arc;
use std::time::Duration;

use backon::ExponentialBuilder;
use reqwest::header::HeaderValue;

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

    /// The indexer's WebSocket origin — host-root `/ws`.
    ///
    /// This is a **separate host** from [`base_url`](Self::base_url): the
    /// `/api/exchange` HTTP gateway does not proxy WebSocket upgrades, so the
    /// stream connects straight to the indexer (the deployment's
    /// `NEXT_PUBLIC_INDEXER_WS_URL`) rather than to a `/ws` path under the REST
    /// base. It therefore cannot be derived from `base_url`.
    ///
    /// Returns `None` for networks whose production WS host is **not yet
    /// confirmed** (ENG-3398). While it is `None`, [`Client::connect_ws`] and
    /// [`Client::connect`] refuse to connect rather than guess a host; supply
    /// the endpoint explicitly with [`Config::with_ws_url`] in the meantime.
    ///
    /// [`Client::connect_ws`]: crate::Client::connect_ws
    /// [`Client::connect`]: crate::Client::connect
    pub fn ws_base(self) -> Option<&'static str> {
        match self {
            // Local dev serves REST and WS from the same indexer process, so
            // the WS origin is this host's `/ws` and is known.
            Network::Local => Some("ws://localhost:9090/ws"),
            // The production / beta indexer WS host is a separate origin that
            // has not been confirmed yet — see ENG-3398. Don't ship a guess.
            Network::Stable | Network::Beta => None,
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

/// Default `User-Agent` the SDK sends on every request, e.g.
/// `nexus-exchange-rs/0.1.0`. The version is taken from the crate version at
/// build time so it never drifts. A descriptive UA lets the server-side request
/// indexer attribute traffic to the Rust SDK (vs CLI, web frontend, or raw
/// callers); applications embedding the SDK can override it via
/// [`Config::with_user_agent`]. Always valid ASCII, so it is a safe fallback.
pub(crate) const DEFAULT_USER_AGENT: &str =
    concat!("nexus-exchange-rs/", env!("CARGO_PKG_VERSION"));

/// Client configuration. Credentials are optional — public market-data
/// endpoints need none.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: String,
    /// The WebSocket origin to stream from, or `None` when it is not known for
    /// the configured network (production host unconfirmed — ENG-3398). A
    /// separate host from `base_url`; see [`Network::ws_base`].
    pub(crate) ws_url: Option<String>,
    pub(crate) ws: WsConfig,
    pub(crate) rate_limit: RateLimit,
    pub(crate) credentials: Option<Arc<dyn Credential>>,
    pub(crate) nonce: Arc<dyn Nonce>,
    pub(crate) timeout: Duration,
    pub(crate) retry: RetryConfig,
    pub(crate) user_agent: String,
}

impl Config {
    /// Target the given [`Network`], unauthenticated.
    pub fn new(network: Network) -> Self {
        Self {
            base_url: network.base_url().to_string(),
            ws_url: network.ws_base().map(str::to_string),
            ws: WsConfig::default(),
            rate_limit: RateLimit::default(),
            credentials: None,
            nonce: Arc::new(SystemTimeNonce),
            timeout: DEFAULT_TIMEOUT,
            retry: RetryConfig::default(),
            user_agent: DEFAULT_USER_AGENT.to_string(),
        }
    }

    /// Target a custom REST base URL (e.g. a preview deployment),
    /// unauthenticated.
    ///
    /// No WebSocket URL is inferred: the stream lives on a separate host that
    /// cannot be derived from the REST base (see [`Network::ws_base`]). To
    /// stream against a custom deployment, set it explicitly with
    /// [`Config::with_ws_url`]; otherwise [`Client::connect`] /
    /// [`Client::connect_ws`] report that no endpoint is configured rather than
    /// connect to a guessed host.
    ///
    /// [`Client::connect`]: crate::Client::connect
    /// [`Client::connect_ws`]: crate::Client::connect_ws
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        Self {
            base_url,
            ws_url: None,
            ws: WsConfig::default(),
            rate_limit: RateLimit::default(),
            credentials: None,
            nonce: Arc::new(SystemTimeNonce),
            timeout: DEFAULT_TIMEOUT,
            retry: RetryConfig::default(),
            user_agent: DEFAULT_USER_AGENT.to_string(),
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

    /// Override the `User-Agent` sent on every request (REST and the WebSocket
    /// handshake).
    ///
    /// Applications built on top of the SDK should set this to identify
    /// themselves to the server-side request indexer (e.g. `nexus-cli/1.2.0` or
    /// `nexus-web/2026.06`), which is what lets traffic be broken down by
    /// client. Defaults to `nexus-exchange-rs/<version>`.
    ///
    /// The value is normalized here: one that is not a valid HTTP header value
    /// (visible ASCII, no control characters) is replaced with the default UA
    /// at construction, so [`user_agent`](Self::user_agent) and the bytes put
    /// on the wire can never disagree, and this can never fail the build.
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        let user_agent = user_agent.into();
        self.user_agent = if HeaderValue::from_str(&user_agent).is_ok() {
            user_agent
        } else {
            DEFAULT_USER_AGENT.to_string()
        };
        self
    }

    /// Set the WebSocket origin to stream from (host-root `/ws` — a separate
    /// host from the REST base; see [`Network::ws_base`]). Required to stream
    /// on any network whose WS host is not yet built in.
    pub fn with_ws_url(mut self, ws_url: impl Into<String>) -> Self {
        self.ws_url = Some(ws_url.into());
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
        self.credentials = Some(Credentials::api_key(key_id, secret).into_arc());
        self
    }

    /// Authenticate with a session bearer token from `POST /auth/login`.
    pub fn session_token(mut self, token: impl Into<String>) -> Self {
        self.credentials = Some(Credentials::session(token).into_arc());
        self
    }

    /// Authenticate with a custom [`Credential`] implementation.
    pub fn with_credential(mut self, credential: Arc<dyn Credential>) -> Self {
        self.credentials = Some(credential);
        self
    }

    /// Override the [`Nonce`] source used to timestamp signed requests. Defaults
    /// to [`SystemTimeNonce`].
    pub fn with_nonce(mut self, nonce: Arc<dyn Nonce>) -> Self {
        self.nonce = nonce;
        self
    }

    /// The configured REST base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The configured WebSocket origin, or `None` if none is known for this
    /// network yet (see [`Network::ws_base`]).
    pub fn ws_url(&self) -> Option<&str> {
        self.ws_url.as_deref()
    }

    /// The configured `User-Agent`.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
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

    /// The WS origin is a separate host, never the `/api/exchange` REST
    /// gateway (which can't proxy WS upgrades). Local is known; the production
    /// hosts are unconfirmed (ENG-3398) and must surface as `None` rather than
    /// a guessed URL.
    #[test]
    fn ws_base_is_known_only_for_local() {
        assert_eq!(Network::Local.ws_base(), Some("ws://localhost:9090/ws"));
        assert_eq!(Network::Stable.ws_base(), None);
        assert_eq!(Network::Beta.ws_base(), None);
    }

    /// `Config` mirrors `ws_base`: a network with a known WS host carries it,
    /// and an unconfirmed one leaves `ws_url` unset rather than derived from
    /// the REST base.
    #[test]
    fn config_ws_url_follows_network_and_is_not_derived_from_rest_base() {
        assert_eq!(
            Config::new(Network::Local).ws_url(),
            Some("ws://localhost:9090/ws")
        );
        assert_eq!(Config::new(Network::Stable).ws_url(), None);
        // A custom REST base does not imply a WS host.
        assert_eq!(
            Config::with_base_url("https://preview.example/api/exchange").ws_url(),
            None
        );
        // ...until set explicitly.
        assert_eq!(
            Config::with_base_url("https://preview.example/api/exchange")
                .with_ws_url("wss://ws.preview.example/ws")
                .ws_url(),
            Some("wss://ws.preview.example/ws")
        );
    }

    #[test]
    fn channel_capacity_is_clamped_to_at_least_one() {
        let cfg = Config::default().with_channel_capacity(0);
        assert_eq!(cfg.ws.channel_capacity, 1);
    }
}
