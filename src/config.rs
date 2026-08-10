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

/// The EIP-712 signing domain for a [`Network`].
///
/// Spelled the same as the spec's `x-nexus-networks[*].signing_domain` and the
/// `SigningDomain` schema, so one name means one thing across the static map,
/// the runtime `/metadata` payload, and generated clients.
///
/// # `chain_id` is deliberately absent
///
/// [`chain_id`](Self::chain_id) is **always `None` here**, and that means "this
/// SDK does not publish the value" — *not* that it is zero. The signing domain
/// is per-network and server-authoritative. A client that cannot obtain a chain
/// id must **refuse to sign** rather than guess or default: a wrong domain
/// either fails verification or, worse, produces a signature that is valid on a
/// *different* network.
///
/// This is why [`EthSigner::register_agent`](crate::EthSigner::register_agent)
/// takes `chain_id` as an explicit argument and has no default — there is no
/// safe value for the SDK to supply on the caller's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SigningDomain {
    /// EIP-712 domain `name` — `"Nexus Exchange"` on every network.
    pub name: &'static str,
    /// EIP-712 domain `version` — `"1"` on every network.
    pub version: &'static str,
    /// EIP-712 domain `chainId`, or `None` when unpublished — always `None`.
    /// See the type-level note: do not substitute a default.
    pub chain_id: Option<u64>,
}

/// Which Nexus Exchange **network** to target.
///
/// The public axis is [`Testnet`](Self::Testnet) (play funds) versus
/// [`Mainnet`](Self::Mainnet) (real funds). [`Local`](Self::Local) is a
/// developer convenience, not a public network — and never a fallback when a
/// public host fails to resolve, since silently succeeding against localhost
/// hides a misconfigured client.
///
/// # Never derive a host by interpolating the network name
///
/// Mainnet is deliberately **off-pattern** — `api.nexus.xyz`, not
/// `api.mainnet.nexus.xyz`. A template like `api.{network}.nexus.xyz` resolves
/// for every environment that *can* be tested and fails only on real funds,
/// which is the one environment that cannot be rehearsed. Every arm below is
/// therefore written out as a named case; keep it that way.
///
/// # Credentials never cross networks
///
/// Session tokens, HMAC API keys and agent keys are minted **per network** and
/// are invalid on any other, so a key leaked or misconfigured on testnet cannot
/// sign for real funds. Build a separate [`Config`] per network; never carry a
/// signature, nonce or agent registration across them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Network {
    /// **Real funds.** Collateral is USDX bridged from Ethereum Mainnet; every
    /// order moves real money and there is no faucet.
    ///
    /// # Not targetable by this release
    ///
    /// Selecting `Mainnet` builds a [`Config`], but every request through the
    /// resulting [`Client`](crate::Client) is **rejected locally** before any
    /// bytes leave the process. Two independent reasons, either sufficient:
    ///
    /// - `api.nexus.xyz` does not resolve yet (DNS/TLS is separate infra work —
    ///   ENG-8155).
    /// - Its durable base carries the version in the base (`…/v1`) rather than
    ///   in the path, which is not the dual-stack path layout this SDK builds
    ///   and signs (see [`Config::direct_base_url`]). Sending the current
    ///   layout there would produce wrong URLs and a signature over a path the
    ///   server never sees.
    ///
    /// Guessing either one against a real-funds host is exactly the failure the
    /// network axis exists to prevent, so the SDK fails closed and loudly
    /// instead. Tracked by ENG-6452's follow-up.
    Mainnet,
    /// **Play funds** — balances are synthetic USDX credited by the faucet and
    /// carry no real-world value. The safe target for integration work and CI,
    /// and the [`Default`] for [`Config`].
    ///
    /// Served today by the legacy gateway base `exchange.nexus.xyz`. That host
    /// is testnet: its traffic migrates to `api.testnet.nexus.xyz` and **never**
    /// to the bare `api.nexus.xyz`, which is real funds.
    Testnet,
    /// A locally run indexer. Play funds, faucet available. Not a public
    /// network and not a deployment target.
    Local,
}

impl Network {
    /// Legacy gateway base URL for this network (the `/api/exchange` REST
    /// gateway). Routes that have **not** yet migrated to the direct `/api/v1`
    /// service are still served here (dual-stack — ENG-4751).
    ///
    /// For [`Mainnet`](Self::Mainnet) this reports the documented durable base
    /// for completeness; requests are refused before it is ever used. See the
    /// variant docs.
    pub fn base_url(self) -> &'static str {
        // Named cases, never interpolated — see the type-level note.
        match self {
            Network::Mainnet => "https://api.nexus.xyz/v1",
            Network::Testnet => "https://exchange.nexus.xyz/api/exchange",
            Network::Local => "http://localhost:9090",
        }
    }

    /// Base URL for the direct-service `/api/v1` surface.
    ///
    /// Under the gateway-elimination work (ENG-4740) each backend service
    /// exposes its own REST API under an `/api/v1` prefix. Requests to
    /// `/api/v1/*` paths are routed here; see [`crate::Client`] for how the base
    /// is selected per request.
    ///
    /// # This is *not* the host root
    ///
    /// It reads as though it should be — the migration is described as moving to
    /// "the host root" — but on every deployment that exists today the `/api/v1`
    /// surface is mounted **under the gateway base**, so this equals
    /// [`base_url`]. Measured on testnet:
    ///
    /// ```text
    /// https://exchange.nexus.xyz/api/exchange/api/v1/markets/summary  -> 200 (JSON)
    /// https://exchange.nexus.xyz/api/v1/markets/summary               -> 404 (frontend HTML)
    /// ```
    ///
    /// The gateway recognizes `/api/v1` specifically: `/api/v2` and any other
    /// junk segment answer `404` with a JSON `NOT_FOUND` body, so it is a real
    /// mount rather than a permissive router. Pointing this at the host root —
    /// which is what this method returned until now — sends every `/api/v1`
    /// request to the marketing frontend, which answers `404` with an HTML body.
    ///
    /// The method survives the correction because the split is still real: when
    /// the direct surface does move off the gateway it moves *per deployment*,
    /// and [`Config::with_direct_base_url`] retargets it without touching any
    /// path literal.
    ///
    /// # Comparing this against the other Nexus SDKs
    ///
    /// The two-base split here is an artifact of being **dual-stack**, not a
    /// different target: some routes still live on the gateway, so the base has
    /// to be chosen per path. An SDK that only speaks the `/api/v1` surface
    /// needs no such choice and can fold the prefix into a single base instead.
    /// The field names therefore do *not* line up one-to-one, and the pairing
    /// that matters is:
    ///
    /// ```text
    /// this SDK / Python:  direct_base_url + "/api/v1/orders"
    /// TypeScript:         baseUrl (= gateway base + "/api/v1") + "/orders"
    /// ```
    ///
    /// Both compose to `https://exchange.nexus.xyz/api/exchange/api/v1/orders`
    /// and both sign the **full** path including `/api/v1` but *excluding* the
    /// gateway prefix, which the gateway strips before the indexer verifies. So
    /// TypeScript's `baseUrl` is the analogue of this method plus the prefix —
    /// it is *not* the analogue of [`base_url`]. Reading the two
    /// `base_url`-shaped fields as the same thing is the one way to conclude
    /// that a prefix disagrees when it does not.
    ///
    /// [`base_url`]: Self::base_url
    pub fn direct_base_url(self) -> &'static str {
        // Named cases, never interpolated — see the type-level note.
        match self {
            // Mainnet's durable base already carries `/v1`; there is no separate
            // direct surface. Reported for completeness only — requests to this
            // network are refused. See the `Mainnet` variant docs.
            Network::Mainnet => "https://api.nexus.xyz/v1",
            // The gateway base, NOT the host root: `/api/v1` is mounted under
            // `/api/exchange` on this deployment. See the method docs.
            Network::Testnet => "https://exchange.nexus.xyz/api/exchange",
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
    /// Returns `None` for networks whose WS host is **not yet usable**. While
    /// it is `None`, [`Client::connect_ws`] and [`Client::connect`] refuse to
    /// connect rather than guess a host; supply the endpoint explicitly with
    /// [`Config::with_ws_url`] in the meantime.
    ///
    /// [`Client::connect_ws`]: crate::Client::connect_ws
    /// [`Client::connect`]: crate::Client::connect
    pub fn ws_base(self) -> Option<&'static str> {
        match self {
            // Local dev serves REST and WS from the same indexer process, so
            // the WS origin is this host's `/ws` and is known.
            Network::Local => Some("ws://localhost:9090/ws"),
            // Testnet still has no usable WS origin. The spec's per-network map
            // does publish one (`wss://api.testnet.nexus.xyz`), but that is a
            // *different origin* from the legacy gateway this network's REST
            // still targets, and it does not resolve yet. The upgrade token is
            // minted over REST (`POST /ws/token`) and is scoped to the origin
            // that issued it, so pairing the legacy REST host with that WS host
            // would send a token to a server that never issued it. Both move
            // together or neither does — ENG-3398.
            Network::Testnet => None,
            // Mainnet is not targetable at all in this release; see the variant
            // docs. Nothing to connect to, and nothing to guess.
            Network::Mainnet => None,
        }
    }

    /// Whether this network moves **real collateral**, as opposed to one where
    /// funding comes from the synthetic faucet
    /// ([`Client::claim_credit`](crate::Client::claim_credit)).
    ///
    /// This is the safety predicate behind [`Client::fund`](crate::Client::fund):
    /// the convenience helper claims faucet credit on play-funds networks but
    /// refuses to silently deposit real collateral. The match is exhaustive on
    /// purpose — a new [`Network`] variant must consciously declare whether it
    /// is real-money before any helper will fund it, and the fail-safe answer
    /// for anything unrecognized is *real funds*.
    ///
    /// Renamed from `is_production` in 0.8.0: the old name conflated "the
    /// deployment we call production" with "moves real money", and the legacy
    /// `exchange.nexus.xyz` host it returned `true` for is in fact **testnet**.
    pub fn is_mainnet(self) -> bool {
        match self {
            Network::Mainnet => true,
            Network::Testnet | Network::Local => false,
        }
    }

    /// The EIP-712 [`SigningDomain`] for this network.
    ///
    /// `name` and `version` are the values the contract has always documented
    /// for `POST /agents/register`. `chain_id` is **always `None`** — it is
    /// server-authoritative and must be read from `/metadata` for the network
    /// you are connected to. See [`SigningDomain`] for why a default would be
    /// dangerous rather than merely wrong.
    pub fn signing_domain(self) -> SigningDomain {
        // Identical across networks today, but returned per-network so a future
        // divergence is a one-line change here rather than a hunt through
        // call sites. Sourced from the auth module so the constants that
        // actually sign and the constants we advertise cannot drift apart.
        let _ = self;
        SigningDomain {
            name: crate::auth::eth::EIP712_DOMAIN_NAME,
            version: crate::auth::eth::EIP712_DOMAIN_VERSION,
            chain_id: None,
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
    /// [`TransientError::RateLimited`](crate::TransientError::RateLimited).
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

/// How the client retries [transient](crate::Error::is_retryable) failures on
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

/// Raw contents of the repo-root `.api-version` file — the spec tag this crate
/// is pinned to (e.g. `v0.6.2\n`). Embedded at compile time so it tracks the
/// pin and never drifts (the same file the spec-drift gate reads); the path is
/// relative to this source file, so `../.api-version` resolves to the repo
/// root. The trailing newline is trimmed at the point the header value is built
/// (`trim` is not `const`).
pub(crate) const API_VERSION_RAW: &str = include_str!("../.api-version");

/// Name of the header carrying the pinned spec tag ([`API_VERSION_RAW`]) on
/// every request, so the server-side request indexer can meter edge usage per
/// spec version (ENG-4804). HTTP header names are case-insensitive.
pub(crate) const API_VERSION_HEADER: &str = "X-Nexus-Api-Version";

/// Derive the direct-service base for the `/api/v1` surface from a REST base
/// URL. The two are the **same base**: `/api/v1` is mounted under the gateway
/// prefix, not at the host root, so the only correct derivation is the identity
/// (bar a trailing slash, which would otherwise double up when a path is joined).
///
/// This used to strip a trailing `/api/exchange`, which produced a base that
/// serves no API at all: `https://exchange.nexus.xyz/api/v1/...` is the
/// marketing frontend and answers `404` with an HTML body, while
/// `https://exchange.nexus.xyz/api/exchange/api/v1/...` is the live surface.
/// Stripping therefore broke every `/api/v1` route in the client — see
/// [`Network::direct_base_url`] for the measurements.
///
/// When a deployment genuinely serves the direct surface on another host,
/// override it with [`Config::with_direct_base_url`]; that is the supported way
/// to express a split, rather than inferring one from the base's shape.
fn derive_direct_base(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

/// Client configuration. Credentials are optional — public market-data
/// endpoints need none.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: String,
    /// Host-root base for the direct-service `/api/v1` surface (see
    /// [`Network::direct_base_url`]). Requests whose path begins with `/api/v1/`
    /// are sent here instead of [`base_url`](Self::base_url); everything else
    /// stays on the legacy gateway base.
    pub(crate) direct_base_url: String,
    /// The [`Network`] this client targets, when built via [`Config::new`].
    /// `None` when built from a raw base URL ([`Config::with_base_url`]), where
    /// the real-money-vs-faucet character of the host is unknown — see
    /// [`Config::network`].
    pub(crate) network: Option<Network>,
    /// The WebSocket origin to stream from, or `None` when it is not known for
    /// the configured network (no usable WS origin yet — ENG-3398). A
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
            direct_base_url: network.direct_base_url().to_string(),
            network: Some(network),
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
        let direct_base_url = derive_direct_base(&base_url);
        Self {
            base_url,
            direct_base_url,
            network: None,
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
    /// timed-out attempt is [transient](crate::Error::is_retryable) and so is
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

    /// Override the base URL used for the `/api/v1` surface (see
    /// [`Network::direct_base_url`]).
    ///
    /// [`Config::with_base_url`] uses the REST base unchanged, because `/api/v1`
    /// is mounted under the gateway prefix on every deployment that exists
    /// today. Use this setter when a deployment genuinely serves the direct
    /// service elsewhere — e.g. once gateway elimination (ENG-4740) moves it to
    /// its own host.
    ///
    /// Retargeting the base does **not** change what is signed: the canonical
    /// path is the `/api/v1/...` literal, independent of the base. That holds as
    /// long as the new base's own path segment is stripped before the indexer
    /// verifies, as the gateway does with `/api/exchange`. Against a base that
    /// does not strip, the signature would cover a path the server never sees —
    /// verify with one authenticated call before trusting a new host.
    pub fn with_direct_base_url(mut self, direct_base_url: impl Into<String>) -> Self {
        self.direct_base_url = direct_base_url.into();
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

    /// The configured (legacy gateway) REST base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The configured direct-service base URL for the `/api/v1` surface (see
    /// [`Network::direct_base_url`]).
    pub fn direct_base_url(&self) -> &str {
        &self.direct_base_url
    }

    /// The [`Network`] this client targets, or `None` when it was built from a
    /// raw base URL via [`Config::with_base_url`] (the host's real-money vs.
    /// testnet-faucet character is then unknown to the SDK).
    pub fn network(&self) -> Option<Network> {
        self.network
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
    /// Targets [`Network::Testnet`] — play funds. The default must never be a
    /// real-funds network: reaching mainnet has to be a deliberate, typed
    /// choice, not what you get by omission.
    fn default() -> Self {
        Self::new(Network::Testnet)
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
    /// gateway (which can't proxy WS upgrades). Local is known; the others must
    /// surface as `None` rather than a guessed URL — in particular testnet must
    /// not be paired with the durable `api.testnet.nexus.xyz` WS host while its
    /// REST still lives on the legacy origin (ENG-3398).
    #[test]
    fn ws_base_is_known_only_for_local() {
        assert_eq!(Network::Local.ws_base(), Some("ws://localhost:9090/ws"));
        assert_eq!(Network::Testnet.ws_base(), None);
        assert_eq!(Network::Mainnet.ws_base(), None);
    }

    /// Only mainnet moves real collateral; the others fund from the faucet.
    /// `fund()` keys its real-money safety guard off this.
    #[test]
    fn only_mainnet_is_real_funds() {
        assert!(Network::Mainnet.is_mainnet());
        assert!(!Network::Testnet.is_mainnet());
        assert!(!Network::Local.is_mainnet());
    }

    /// Mainnet's host is deliberately **off-pattern**. `api.{network}.nexus.xyz`
    /// would resolve for every environment that can be tested and fail only on
    /// real funds — the one environment that cannot be rehearsed. This test
    /// exists so a future "tidy-up" into an interpolated host fails loudly.
    #[test]
    fn mainnet_host_is_not_interpolated_from_the_network_name() {
        for url in [
            Network::Mainnet.base_url(),
            Network::Mainnet.direct_base_url(),
        ] {
            assert!(
                url.starts_with("https://api.nexus.xyz"),
                "mainnet must be the bare api.nexus.xyz host, got {url}"
            );
            assert!(
                !url.contains("mainnet."),
                "mainnet host must not be derived by interpolation, got {url}"
            );
        }
        // ...and testnet must never collapse onto the real-funds host.
        for url in [
            Network::Testnet.base_url(),
            Network::Testnet.direct_base_url(),
        ] {
            assert!(
                !url.starts_with("https://api.nexus.xyz"),
                "testnet must never point at the real-funds host, got {url}"
            );
        }
    }

    /// The EIP-712 domain is per-network, and `chain_id` is deliberately
    /// unpublished: a default would let a client sign under the wrong domain,
    /// which can yield a signature valid on a *different* network.
    #[test]
    fn signing_domain_never_supplies_a_chain_id() {
        for network in [Network::Mainnet, Network::Testnet, Network::Local] {
            let domain = network.signing_domain();
            assert_eq!(domain.name, "Nexus Exchange");
            assert_eq!(domain.version, "1");
            assert_eq!(
                domain.chain_id, None,
                "{network:?} must not default a chain id"
            );
        }
    }

    /// Reaching real funds must be a deliberate, typed choice — never what a
    /// caller gets by omission.
    #[test]
    fn default_config_is_play_funds() {
        let default = Config::default();
        assert_eq!(default.network(), Some(Network::Testnet));
        assert!(!default.network().expect("network").is_mainnet());
    }

    /// A network-built config carries its `Network`; a raw-base-URL one does
    /// not (so `fund()` can tell "known testnet" from "unknown host").
    #[test]
    fn config_retains_network_only_when_built_from_one() {
        assert_eq!(
            Config::new(Network::Testnet).network(),
            Some(Network::Testnet)
        );
        assert_eq!(Config::with_base_url("http://x").network(), None);
    }

    /// `Config` mirrors `ws_base`: a network with a known WS host carries it,
    /// and one without a usable origin leaves `ws_url` unset rather than
    /// derived from the REST base.
    #[test]
    fn config_ws_url_follows_network_and_is_not_derived_from_rest_base() {
        assert_eq!(
            Config::new(Network::Local).ws_url(),
            Some("ws://localhost:9090/ws")
        );
        assert_eq!(Config::new(Network::Testnet).ws_url(), None);
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

    /// Built-in networks carry both bases, and on every deployment that exists
    /// today they are the **same** base: `/api/v1` is mounted under the gateway
    /// prefix, not at the host root.
    ///
    /// Testnet keeps the **legacy** host on purpose. The spec's durable
    /// `api.testnet.nexus.xyz` base does not resolve yet, and moving to it also
    /// changes the path layout (`/v1` in the base), so it is a separate change.
    #[test]
    fn networks_expose_gateway_and_direct_bases() {
        assert_eq!(
            Network::Testnet.base_url(),
            "https://exchange.nexus.xyz/api/exchange"
        );
        assert_eq!(
            Network::Testnet.direct_base_url(),
            "https://exchange.nexus.xyz/api/exchange"
        );
        // Local dev serves both surfaces from one origin.
        assert_eq!(Network::Local.base_url(), Network::Local.direct_base_url());
    }

    /// The `/api/v1` surface must resolve to an **absolute URL that carries the
    /// gateway prefix**, for every built-in network.
    ///
    /// This is the assertion whose absence let the bug ship. The tests around it
    /// checked the base and the path prefix separately, and both halves were
    /// individually defensible while the composed URL pointed at a host that
    /// serves no API. Asserting the resolved URL is what fails when the two
    /// agree with each other but disagree with the deployment.
    #[test]
    fn v1_surface_resolves_under_the_gateway_prefix() {
        assert_eq!(
            format!(
                "{}{}",
                Network::Testnet.direct_base_url(),
                "/api/v1/markets/summary"
            ),
            "https://exchange.nexus.xyz/api/exchange/api/v1/markets/summary",
        );
        // A custom gateway-style base keeps its prefix rather than losing it.
        assert_eq!(
            format!(
                "{}{}",
                Config::with_base_url("https://preview.example/api/exchange").direct_base_url(),
                "/api/v1/orders"
            ),
            "https://preview.example/api/exchange/api/v1/orders",
        );
    }

    /// `with_base_url` uses one base for both surfaces: `/api/v1` is mounted
    /// under the gateway prefix, so the direct base keeps it rather than
    /// stripping it. A bare origin is likewise used unchanged (this is what
    /// keeps the wiremock tests, which pass a bare `http://127.0.0.1:PORT`,
    /// working).
    #[test]
    fn direct_base_keeps_the_gateway_prefix() {
        let cfg = Config::with_base_url("https://preview.example/api/exchange");
        assert_eq!(cfg.base_url(), "https://preview.example/api/exchange");
        assert_eq!(
            cfg.direct_base_url(),
            "https://preview.example/api/exchange"
        );

        // Trailing slash trimmed, so joining a path never doubles the separator.
        assert_eq!(
            Config::with_base_url("https://preview.example/api/exchange/").direct_base_url(),
            "https://preview.example/api/exchange"
        );

        // A bare origin (no gateway segment) is used unchanged for both.
        let bare = Config::with_base_url("http://127.0.0.1:8080");
        assert_eq!(bare.base_url(), "http://127.0.0.1:8080");
        assert_eq!(bare.direct_base_url(), "http://127.0.0.1:8080");

        // Explicit override wins over the derivation.
        assert_eq!(
            Config::with_base_url("https://preview.example/api/exchange")
                .with_direct_base_url("https://direct.preview.example")
                .direct_base_url(),
            "https://direct.preview.example"
        );
    }
}
