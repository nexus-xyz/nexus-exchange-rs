//! Client configuration.

use std::time::Duration;

use backon::ExponentialBuilder;

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
}

/// How the client retries [transient](crate::Error::is_transient) failures.
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
/// Only transient errors are retried; deterministic failures surface
/// immediately. Construct the [`Default`], or tune fields directly:
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
    /// Upper bound on any single backoff delay.
    pub max_delay: Duration,
    /// Multiplier applied to the delay after each attempt. Should be `>= 1.0`;
    /// values below `1.0` (or `NaN`) shrink the delay each step and degrade the
    /// backoff.
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
        let builder = ExponentialBuilder::default()
            .with_min_delay(self.min_delay)
            .with_max_delay(self.max_delay)
            .with_factor(self.factor)
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

/// Client configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: String,
    pub(crate) timeout: Duration,
    pub(crate) retry: RetryConfig,
}

/// Default per-request timeout. Generous enough for cold connections, tight
/// enough to surface a stalled request rather than hang indefinitely.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

impl Config {
    /// Target the given [`Network`].
    pub fn new(network: Network) -> Self {
        Self {
            base_url: network.base_url().to_string(),
            timeout: DEFAULT_TIMEOUT,
            retry: RetryConfig::default(),
        }
    }

    /// Target a custom base URL (e.g. a preview deployment).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: DEFAULT_TIMEOUT,
            retry: RetryConfig::default(),
        }
    }

    /// Set the per-request timeout. This bounds each individual attempt; a
    /// timed-out attempt is [transient](crate::Error::is_transient) and so is
    /// subject to retry. Because it is per-attempt, a retried call can take a
    /// multiple of this value — see [`RetryConfig`] for the total-time bound.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configure how transient failures are retried. Pass
    /// [`RetryConfig::disabled`] to turn retries off.
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new(Network::Stable)
    }
}
