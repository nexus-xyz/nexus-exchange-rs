//! Client configuration.

use crate::auth::Credentials;
use crate::ws::Backoff;
use std::sync::Arc;

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

/// Client configuration. Credentials are optional — public market-data
/// endpoints need none.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: String,
    pub(crate) ws_url: String,
    pub(crate) ws: WsConfig,
    pub(crate) rate_limit: RateLimit,
    pub(crate) credentials: Option<Arc<Credentials>>,
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
        }
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
