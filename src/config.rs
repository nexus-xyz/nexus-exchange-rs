//! Client configuration.

use crate::auth::{Credential, Credentials, Nonce, SystemTimeNonce};
use crate::ws::Backoff;
use crate::{Error, Result};
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
    /// EIP-712 domain `chainId`, or `None` when unpublished. `None` for every
    /// built-in network; a [`CustomNetwork`] may supply one. See the type-level
    /// note: do not substitute a default.
    pub chain_id: Option<u64>,
}

impl SigningDomain {
    /// The signing domain for a deployment whose `chain_id` the **caller** knows
    /// — the only way to obtain a signable domain from this SDK.
    ///
    /// `name` and `version` are contract-level constants, identical on every
    /// deployment, and are supplied from the same source the signer uses so the
    /// two cannot drift. `chain_id` is the part that is per-network and
    /// server-authoritative, so it is the part you must pass: read it from
    /// `GET /metadata` for the host you are pointed at.
    ///
    /// This type is `#[non_exhaustive]`, so this constructor — not a struct
    /// literal — is how a [`CustomNetwork`] receives a domain.
    pub fn new(chain_id: u64) -> Self {
        Self {
            name: crate::auth::eth::EIP712_DOMAIN_NAME,
            version: crate::auth::eth::EIP712_DOMAIN_VERSION,
            chain_id: Some(chain_id),
        }
    }
}

/// Whether a target moves **real collateral**.
///
/// Tri-state on purpose. A boolean forces a default, and both defaults are
/// wrong: `false` makes every guardrail in the client lie in the direction that
/// costs money, `true` makes development unusable. [`Unknown`](Self::Unknown) is
/// the third answer — "nobody declared this" — and it is treated as *unsafe*,
/// not as play funds.
/// Guard money movement by matching [`Play`](Self::Play) positively — or with
/// [`is_known_play`](Self::is_known_play) — rather than negating
/// [`Real`](Self::Real). This enum is `#[non_exhaustive]`, so a future
/// classification lands in your wildcard arm; make that arm the safe one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Funds {
    /// Real collateral. Orders move real money.
    Real,
    /// Synthetic funds with no real-world value (faucet-credited).
    Play,
    /// Not declared by the caller. Real-funds-guarded helpers **refuse** rather
    /// than assume play funds, so an undeclared target can never be mistaken for
    /// a safe one.
    Unknown,
}

impl Funds {
    /// Whether this is *known* to be play funds — the only state in which a
    /// real-funds guard may open. [`Unknown`](Self::Unknown) answers `false`, so
    /// a target nobody classified is treated as dangerous.
    ///
    /// Deliberately not spelled as `!is_real()`: the negation of a tri-state is
    /// how "unknown" silently becomes "safe".
    pub fn is_known_play(self) -> bool {
        matches!(self, Funds::Play)
    }
}

/// A caller-supplied deployment: the whole safety bundle, not just a URL.
///
/// # Why this exists
///
/// A deployment that this crate does not name still has to be reachable from
/// it — your own environment, a preview host, a sandbox. Enumerating such hosts
/// in a **published** client would put them in the package permanently and
/// discoverably, and the list would need extending every time one was added. So
/// the caller supplies the URL and this type ships none.
///
/// # Client-side only
///
/// `Custom` is **not a value the server accepts** and never appears in the
/// spec's `x-nexus-networks`. It names a target for *this process*; nothing
/// about it is transmitted, and it is not a network identifier you can send.
///
/// # It carries the flags, not just the address
///
/// A bare base-URL override — which this SDK has always had as
/// [`Config::with_base_url`] — points the transport somewhere new while leaving
/// the safety metadata behind. That is what makes a client report play-funds
/// guardrails while aimed at a real-funds host. So the [`Funds`] classification
/// is **required and has no default**, the faucet flag defaults to *absent*, and
/// the WS origin and signing domain default to *unknown* rather than guessed.
///
/// # Reaching a real-funds deployment
///
/// A `Custom` with [`Funds::Real`] is targetable, unlike
/// [`Network::Mainnet`](Network::Mainnet). These are not in tension: `Mainnet`
/// is refused because this release cannot *build* correct URLs for its durable
/// base (the version sits in the base, `…/v1`, not in the path), which would
/// sign a path the server never sees — a URL-layout problem, not a funds
/// problem. With `Custom` the caller supplies the URL and therefore owns the
/// layout. What stays guarded is money movement:
/// [`Client::fund`](crate::Client::fund) refuses on anything that is not
/// [`Funds::Play`].
///
/// ```
/// use nexus_exchange::{Config, CustomNetwork, Funds, Network};
///
/// let target = CustomNetwork::new("dev", "https://exchange.example.com/api/exchange", Funds::Play)?
///     .with_faucet(true);
/// let config = Config::new(Network::Custom(target));
/// # Ok::<(), nexus_exchange::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomNetwork {
    label: String,
    base_url: String,
    direct_base_url: String,
    ws_url: Option<String>,
    funds: Funds,
    has_faucet: bool,
    signing_domain: Option<SigningDomain>,
}

impl CustomNetwork {
    /// Describe a deployment: a `label`, a REST `base_url`, and its `funds`
    /// classification. All three are required — see the type docs for why
    /// `funds` has no default.
    ///
    /// The direct `/api/v1` base defaults to `base_url` (on every deployment
    /// that exists today the `/api/v1` surface is mounted *under* the gateway
    /// prefix — see [`Network::direct_base_url`]); override it with
    /// [`with_direct_base_url`](Self::with_direct_base_url) when a deployment
    /// genuinely splits them. The faucet is assumed **absent**, and the WS
    /// origin and signing domain **unknown**, until declared.
    ///
    /// # Errors
    ///
    /// Rejects a `base_url` that is not `http(s)://host…`, that carries
    /// `user:pass@` userinfo, or that carries a query or fragment — see
    /// [`with_ws_url`](Self::with_ws_url) for why those last two are refused
    /// rather than ignored. Also rejects a `label` that is not a safe key for
    /// per-network credential storage. A trailing slash is trimmed.
    pub fn new(
        label: impl Into<String>,
        base_url: impl Into<String>,
        funds: Funds,
    ) -> Result<Self> {
        let label = validate_label(&label.into())?;
        let base_url = validate_url(&base_url.into(), &["https://", "http://"], "base URL")?;
        Ok(Self {
            label,
            direct_base_url: base_url.clone(),
            base_url,
            ws_url: None,
            funds,
            has_faucet: false,
            signing_domain: None,
        })
    }

    /// Point the direct `/api/v1` surface at a different base than the REST
    /// base. Validated exactly like the REST base.
    pub fn with_direct_base_url(mut self, direct_base_url: impl Into<String>) -> Result<Self> {
        self.direct_base_url = validate_url(
            &direct_base_url.into(),
            &["https://", "http://"],
            "direct base URL",
        )?;
        Ok(self)
    }

    /// Declare this deployment's WebSocket origin (a `ws://` or `wss://` URL).
    ///
    /// Left unset, [`Network::ws_base`] reports `None` and the streaming client
    /// refuses to connect rather than guess a host — the WS origin is a
    /// **separate host** from the REST base and cannot be derived from it.
    ///
    /// # Errors
    ///
    /// Rejects a scheme other than `ws`/`wss`, userinfo, and a query or
    /// fragment. A query is refused rather than ignored because the upgrade
    /// token is appended by the streaming client; silently keeping a caller's
    /// `?` would produce a URL whose token is not where the server looks for it.
    pub fn with_ws_url(mut self, ws_url: impl Into<String>) -> Result<Self> {
        self.ws_url = Some(validate_url(
            &ws_url.into(),
            &["wss://", "ws://"],
            "WebSocket URL",
        )?);
        Ok(self)
    }

    /// Declare whether the synthetic faucet
    /// ([`Client::claim_credit`](crate::Client::claim_credit)) exists here.
    /// Assumed absent until declared, so [`Client::fund`](crate::Client::fund)
    /// cannot route to a faucet that is not there.
    pub fn with_faucet(mut self, has_faucet: bool) -> Self {
        self.has_faucet = has_faucet;
        self
    }

    /// Declare the EIP-712 signing domain, built with
    /// [`SigningDomain::new`] from a `chain_id` you read off this host's
    /// `GET /metadata`.
    ///
    /// Left unset, [`Network::signing_domain`] reports `None`, which means
    /// **refuse to sign** — never fall back to a constant. A wrong domain either
    /// fails verification or, worse, yields a signature that is valid on a
    /// different network.
    pub fn with_signing_domain(mut self, signing_domain: SigningDomain) -> Self {
        self.signing_domain = Some(signing_domain);
        self
    }

    /// The caller-supplied label. Identifies this target in diagnostics and is
    /// the key under which per-network credentials are namespaced, which is why
    /// it is required and constrained (see [`new`](Self::new)).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The target behind [`Config::with_base_url`], which predates this type.
    ///
    /// Two deliberate differences from [`new`](Self::new):
    ///
    /// - **Funds are [`Funds::Unknown`].** That is the honest reading of a bare
    ///   URL, and it preserves the old behaviour exactly: a raw base URL has
    ///   always refused [`Client::fund`](crate::Client::fund), previously because
    ///   the network was absent and now because the funds are undeclared. The
    ///   refusal must not depend on which of those encodes it.
    /// - **The URL is not validated.** `with_base_url` returns `Self`, not
    ///   `Result`, so there is nowhere to report a rejection; validating here
    ///   would mean panicking on input this SDK has always accepted. A malformed
    ///   base fails at request time, exactly as before. Callers who want the
    ///   checks have [`CustomNetwork::new`].
    pub(crate) fn from_legacy_base_url(base_url: String) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            label: LEGACY_BASE_URL_LABEL.to_string(),
            direct_base_url: derive_direct_base(&base_url),
            base_url,
            ws_url: None,
            funds: Funds::Unknown,
            has_faucet: false,
            signing_domain: None,
        }
    }
}

/// Which Nexus Exchange **network** to target.
///
/// The public axis is [`Testnet`](Self::Testnet) (play funds) versus
/// [`Mainnet`](Self::Mainnet) (real funds). [`Local`](Self::Local) is a
/// developer convenience, not a public network — and never a fallback when a
/// public host fails to resolve, since silently succeeding against localhost
/// hides a misconfigured client. [`Custom`](Self::Custom) is a deployment the
/// **caller** supplies, for the hosts this public crate deliberately does not
/// name.
///
/// # What a network is, and what it is not
///
/// A `Network` is a bundle of facts about a target — its bases, its WebSocket
/// origin, its signing domain, what its funds are worth — not merely an address.
/// Everything that decides whether an operation is safe reads those facts, so a
/// target that carries an address but not the facts is the shape of every
/// mistake this type exists to prevent. That is why
/// [`Custom`](Self::Custom) carries the whole bundle and why
/// [`funds`](Self::funds) has no default.
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
/// signature, nonce or agent registration across them. A
/// [`Custom`](Self::Custom) target must therefore be labelled — the label is the
/// key its credentials are stored under, so two stages cannot end up sharing one
/// namespace.
///
/// # `Custom` is not `Copy`
///
/// [`Custom`](Self::Custom) carries owned strings, so `Network` is `Clone` but no
/// longer `Copy`, and the URL accessors borrow `self` instead of returning
/// `&'static str`. That is the price of a variant that holds a caller-supplied
/// address rather than a literal, and it is deliberate: the alternative is
/// leaking the string or interning it, and neither is worth it to keep a marker
/// trait.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// A **caller-supplied deployment** — your own environment, a preview host, a
    /// sandbox. Ships no hostname; see [`CustomNetwork`] for the bundle it
    /// carries and why it carries all of it.
    ///
    /// Client-side only: never a value the server accepts, and never present in
    /// the spec's `x-nexus-networks`.
    Custom(CustomNetwork),
}

impl Network {
    /// Legacy gateway base URL for this network (the `/api/exchange` REST
    /// gateway). Routes that have **not** yet migrated to the direct `/api/v1`
    /// service are still served here (dual-stack — ENG-4751).
    ///
    /// For [`Mainnet`](Self::Mainnet) this reports the documented durable base
    /// for completeness; requests are refused before it is ever used. See the
    /// variant docs.
    pub fn base_url(&self) -> &str {
        // Named cases, never interpolated — see the type-level note.
        match self {
            Network::Mainnet => "https://api.nexus.xyz/v1",
            Network::Testnet => "https://exchange.nexus.xyz/api/exchange",
            Network::Local => "http://localhost:9090",
            // Verbatim from the caller. Nothing is appended, rewritten or
            // inferred — see `CustomNetwork`.
            Network::Custom(custom) => &custom.base_url,
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
    pub fn direct_base_url(&self) -> &str {
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
            // Defaults to the caller's REST base, since today's deployments
            // mount `/api/v1` under it; overridden only if the caller split them.
            Network::Custom(custom) => &custom.direct_base_url,
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
    pub fn ws_base(&self) -> Option<&str> {
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
            // Only what the caller declared. The WS origin is a separate host
            // from the REST base and is never derived from it, so an
            // undeclared origin stays `None` and the stream refuses to connect.
            Network::Custom(custom) => custom.ws_url.as_deref(),
        }
    }

    /// Whether this target moves **real collateral**, as opposed to synthetic
    /// funds from the faucet
    /// ([`Client::claim_credit`](crate::Client::claim_credit)).
    ///
    /// This is the safety predicate behind [`Client::fund`](crate::Client::fund):
    /// the convenience helper claims faucet credit on play-funds targets but
    /// refuses to silently deposit real collateral. The match is exhaustive on
    /// purpose — a new [`Network`] variant must consciously declare what it moves
    /// before any helper will fund it.
    ///
    /// # Why this is not a `bool`
    ///
    /// It replaced `is_mainnet()` (itself renamed from `is_production` in 0.8.0)
    /// when [`Custom`](Self::Custom) arrived, because neither a name nor a
    /// boolean can answer honestly for a caller-supplied host:
    ///
    /// - A `Custom` pointed at a real-funds stage is **not** `Mainnet`, so
    ///   `is_mainnet()` answered `false` for it — a guardrail lying in the
    ///   direction that costs money.
    /// - A `Custom` whose caller declared nothing is neither real nor play. There
    ///   is no safe boolean for it, which is what [`Funds::Unknown`] is for.
    ///
    /// Callers guarding money movement must match on [`Funds::Play`] (or use
    /// [`Funds::is_known_play`]) rather than negate the real case, so that
    /// `Unknown` fails closed.
    pub fn funds(&self) -> Funds {
        match self {
            Network::Mainnet => Funds::Real,
            Network::Testnet | Network::Local => Funds::Play,
            // Caller-declared and required at construction; never inferred from
            // the URL, the label, or anything else.
            Network::Custom(custom) => custom.funds,
        }
    }

    /// Whether the synthetic faucet
    /// ([`Client::claim_credit`](crate::Client::claim_credit)) exists on this
    /// target.
    ///
    /// Distinct from [`funds`](Self::funds): "not real money" does not imply "has
    /// a faucet". A private play-funds stage may be seeded by other means, and
    /// [`Client::fund`](crate::Client::fund) must not route to a faucet that is
    /// not there. Assumed **absent** for a [`Custom`](Self::Custom) target until
    /// declared with [`CustomNetwork::with_faucet`].
    pub fn has_faucet(&self) -> bool {
        match self {
            // Real funds, no faucet — collateral is bridged, not credited.
            Network::Mainnet => false,
            Network::Testnet | Network::Local => true,
            Network::Custom(custom) => custom.has_faucet,
        }
    }

    /// A short, stable name for this target, safe to use as a key for
    /// per-network credential storage and in diagnostics.
    ///
    /// Built-in networks answer with their lowercase name; a
    /// [`Custom`](Self::Custom) target answers with its caller-supplied
    /// [`label`](CustomNetwork::label). Provided so callers that need to *name* a
    /// network — the CLI namespaces stored credentials this way — do not have to
    /// match on the enum and therefore cannot silently mishandle a variant added
    /// later.
    pub fn label(&self) -> &str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
            Network::Local => "local",
            Network::Custom(custom) => custom.label(),
        }
    }

    /// The EIP-712 [`SigningDomain`] for this target, or `None` when this SDK
    /// has none for it — which means **refuse to sign**, never fall back to a
    /// constant.
    ///
    /// For a built-in network this is always `Some`: `name` and `version` are the
    /// values the contract has always documented for `POST /agents/register`,
    /// and `chain_id` is `None` because it is server-authoritative and must be
    /// read from `/metadata` for the network you are connected to.
    ///
    /// For a [`Custom`](Self::Custom) target it is whatever the caller declared
    /// with [`CustomNetwork::with_signing_domain`], and `None` when they declared
    /// nothing. `Custom` must not become the hole in the never-guess rule: there
    /// is no host to look the domain up from, so the honest answer for an
    /// undeclared one is "unknown". See [`SigningDomain`] for why a default would
    /// be dangerous rather than merely wrong — a signature made under the wrong
    /// domain may be *valid on a different network*.
    pub fn signing_domain(&self) -> Option<SigningDomain> {
        match self {
            // Identical across the built-in networks today, but returned
            // per-network so a future divergence is a one-line change here rather
            // than a hunt through call sites. Sourced from the auth module so the
            // constants that actually sign and the constants we advertise cannot
            // drift apart.
            Network::Mainnet | Network::Testnet | Network::Local => Some(SigningDomain {
                name: crate::auth::eth::EIP712_DOMAIN_NAME,
                version: crate::auth::eth::EIP712_DOMAIN_VERSION,
                chain_id: None,
            }),
            Network::Custom(custom) => custom.signing_domain,
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

/// Longest accepted [`CustomNetwork`] label. Long enough for any stage name we
/// would plausibly use, short enough that it cannot be a smuggled payload.
const MAX_LABEL_LEN: usize = 64;

/// Label carried by the target [`CustomNetwork::from_legacy_base_url`] builds for
/// [`Config::with_base_url`]. Shared with [`reserved_labels`], which refuses it
/// as a caller-supplied label so a declared target cannot land on the same
/// credential key as the legacy bare-URL one.
const LEGACY_BASE_URL_LABEL: &str = "custom";

/// Validate a caller-supplied [`CustomNetwork`] label and return it trimmed.
///
/// The label is not decoration: the CLI namespaces **stored credentials** by it,
/// so it ends up in a keyring entry or a path. The accepted set is therefore
/// deliberately narrow — ASCII alphanumerics, `-`, `_`, `.` — which excludes the
/// separators (`/`, `\`, `:`) and whitespace that could make one target's label
/// address another target's credentials, and excludes control characters that
/// could corrupt a log line. `.` and `..` are refused outright: they are legal
/// under that character set but name a directory rather than a network.
///
/// A built-in network's own name is refused for the same reason (see
/// [`RESERVED_LABELS`]): under that character set it is a perfectly legal label,
/// and it addresses another target's credentials by *naming* it rather than by
/// pathing to it.
///
/// Rejecting is the whole point. A label that cannot be stored safely must fail
/// here, at construction, rather than at some later write that has to guess.
fn validate_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.is_empty() {
        return Err(Error::invalid_request(
            "custom network needs a non-empty label: it identifies the target and \
             is the key its credentials are stored under",
        ));
    }
    if label.len() > MAX_LABEL_LEN {
        return Err(Error::invalid_request(format!(
            "custom network label must be at most {MAX_LABEL_LEN} characters"
        )));
    }
    if label == "." || label == ".." {
        return Err(Error::invalid_request(
            "custom network label must not be `.` or `..`: it is used as a \
             credential-storage key, not a path",
        ));
    }
    if let Some(bad) = label
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(Error::invalid_request(format!(
            "custom network label may contain only ASCII letters, digits, `-`, `_` \
             and `.` (found {bad:?}): it is used as a credential-storage key"
        )));
    }
    if let Some(reserved) = RESERVED_LABELS
        .iter()
        .find(|reserved| label.eq_ignore_ascii_case(reserved))
    {
        return Err(Error::invalid_request(format!(
            "custom network label {label:?} is reserved: it is the name {reserved:?} \
             already answers to, and per-network credentials are stored under that \
             name, so this target would address another network's keys"
        )));
    }
    Ok(label.to_string())
}

/// Labels a caller may not claim for a [`CustomNetwork`]: every built-in
/// [`Network`]'s own [`label`](Network::label), plus the one
/// [`CustomNetwork::from_legacy_base_url`] uses. Compared case-insensitively by
/// [`validate_label`], since a keyring entry or a path need not be
/// case-sensitive, and `Mainnet` must not slip past a check on `mainnet`.
///
/// The hazard is the same one the traversal and separator refusals in
/// [`validate_label`] exist to stop, reached by naming rather than pathing.
/// `Network::label()` is documented as safe to key per-network credential
/// storage on, and the CLI does exactly that (ENG-9827). A `Custom` target
/// labelled `mainnet` therefore answers the *same key* as
/// [`Network::Mainnet`] while pointing at a caller-supplied host — so it reads
/// and writes the real network's stored credentials. `../other` is refused;
/// this must be too.
///
/// Kept in step with [`Network::label`] by
/// `reserved_labels_cover_every_built_in_network`, which walks the built-in
/// variants and asserts each one's label is listed here — so a network added
/// later fails that test rather than silently becoming claimable. The list is
/// literals rather than calls to `label()` because that method borrows from
/// `&self`, and a `Custom` target's label borrows from its `String` field: only
/// the built-in arms are `'static`, and leaning on that distinction to build a
/// `&'static` array would make this security check depend on a lifetime subtlety
/// instead of on something plain to read.
const RESERVED_LABELS: &[&str] = &["mainnet", "testnet", "local", LEGACY_BASE_URL_LABEL];

/// Validate a caller-supplied URL against `allowed_schemes` and return it with
/// any trailing slash trimmed. `what` names the field in the error message.
///
/// Every rejection here is a request that would otherwise be built wrong rather
/// than merely fail:
///
/// - **Scheme** must be one of `allowed_schemes`. Anything else (`file:`,
///   `data:`, a bare host) is not a thing this client can talk to, and an
///   unexpected scheme is how a URL becomes a local-file read.
/// - **Userinfo** (`user:pass@host`) is refused rather than stripped. Credentials
///   belong in the signing path, not the URL; a URL carrying them leaks them into
///   every log, metric label and error message that prints the base.
/// - **Query and fragment** are refused because URLs are built here as
///   `base + path`, so a `?` or `#` in the base does not compose — it swallows
///   the path. The request would go somewhere other than where the signature
///   says, which fails as a signature error rather than as an obvious bad URL.
/// - **Whitespace and control characters** are refused so a base can never
///   inject a newline into a header or split a log line.
///
/// Note what is *not* checked: the host itself. No allowlist, no denylist, no
/// "does this look like one of ours" — that is the entire point of `Custom`, and
/// a hostname check here would put a private host back in this public artifact.
fn validate_url(url: &str, allowed_schemes: &[&str], what: &str) -> Result<String> {
    let url = url.trim();
    let rest = allowed_schemes
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
        .ok_or_else(|| {
            Error::invalid_request(format!(
                "custom network {what} must start with {} (got {url:?})",
                allowed_schemes.join(" or ")
            ))
        })?;
    // Authority runs to the first `/`; the remainder is an optional path.
    // `split` always yields at least one element, so this cannot be empty-handed.
    let authority = rest.split('/').next().unwrap_or_default();
    // Checked without the optional `:port`, because the authority being non-empty
    // is not the same as there being a host: `https://:8080` has a port and no
    // host, contains no `@` for the userinfo check to catch, and would otherwise
    // be accepted here and refused only later, at request time, as an opaque
    // URL-parse error instead of naming the reason. Splitting at the first `:`
    // leaves an IPv6 literal (`[::1]:8080`) non-empty, so those still pass.
    if authority.split(':').next().unwrap_or_default().is_empty() {
        return Err(Error::invalid_request(format!(
            "custom network {what} has no host (got {url:?})"
        )));
    }
    if authority.contains('@') {
        return Err(Error::invalid_request(format!(
            "custom network {what} must not embed credentials (`user:pass@host`): \
             they would leak into every log and error that prints the base"
        )));
    }
    // Matched as a `char` rather than a byte index so the message can never slice
    // a multi-byte boundary.
    if let Some(bad) = url.chars().find(|c| matches!(c, '?' | '#')) {
        return Err(Error::invalid_request(format!(
            "custom network {what} must not carry a query or fragment (found {bad:?}): \
             request URLs are built as base + path, so it would swallow the path"
        )));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::invalid_request(format!(
            "custom network {what} must not contain whitespace or control characters"
        )));
    }
    Ok(url.trim_end_matches('/').to_string())
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
    /// The [`Network`] this client targets. Always present: a raw base URL
    /// ([`Config::with_base_url`]) becomes a [`Network::Custom`] whose funds are
    /// [`Funds::Unknown`], so "which target is this" and "what does it move" are
    /// separate questions with separate answers, instead of both being encoded as
    /// an absent network. See [`Config::network`].
    pub(crate) network: Network,
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
    ///
    /// Every base is taken from the network — including a [`Network::Custom`],
    /// whose URLs were validated when it was constructed.
    pub fn new(network: Network) -> Self {
        Self {
            base_url: network.base_url().to_string(),
            direct_base_url: network.direct_base_url().to_string(),
            ws_url: network.ws_base().map(str::to_string),
            network,
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
    /// A trailing slash is trimmed from the base before any path is joined:
    /// request URLs are built as `base + path` with `path` carrying its own
    /// leading slash, so `…/api/exchange/` would otherwise send `…//orders`. That
    /// is a different path than the `/orders` the client signs, so it would fail
    /// verification rather than merely look untidy.
    ///
    /// # Prefer [`Network::Custom`] for anything but a throwaway target
    ///
    /// This is now sugar for a [`CustomNetwork`] with [`Funds::Unknown`] and no
    /// faucet, WS origin or signing domain — which is all a bare URL can honestly
    /// say. [`Config::network`] therefore reports a `Custom` target rather than
    /// nothing, and every guard reads the same fields for both paths, so there is
    /// no second code path that can drift.
    ///
    /// Because the funds are undeclared, [`Client::fund`](crate::Client::fund)
    /// refuses — as it always has for a raw base URL. Construct a
    /// [`CustomNetwork`] instead to declare what the host moves and to get the
    /// URL validated.
    ///
    /// [`Client::connect`]: crate::Client::connect
    /// [`Client::connect_ws`]: crate::Client::connect_ws
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self::new(Network::Custom(CustomNetwork::from_legacy_base_url(
            base_url.into(),
        )))
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

    /// The [`Network`] this client targets — always known.
    ///
    /// This used to return `Option<Network>`, with `None` meaning "built from a
    /// raw base URL, so we know nothing about the host". That conflated two
    /// separate facts, and the dangerous half was silent: code asking *which
    /// target is this* got `None` and code asking *does this move real money* had
    /// to infer the answer from the same `None`. A raw base URL is now a
    /// [`Network::Custom`] whose [`funds`](Network::funds) are
    /// [`Funds::Unknown`], so the second question has its own answer and every
    /// guard reads it explicitly.
    ///
    /// This reports the **declared target**. The URLs requests actually go to are
    /// [`Config::base_url`] and [`Config::direct_base_url`], which
    /// [`Config::with_direct_base_url`] can override after the fact — so read
    /// those two, not `network().base_url()`, when you want the effective address.
    pub fn network(&self) -> &Network {
        &self.network
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
        assert_eq!(Network::Mainnet.funds(), Funds::Real);
        assert_eq!(Network::Testnet.funds(), Funds::Play);
        assert_eq!(Network::Local.funds(), Funds::Play);

        // Faucet availability is a *separate* question from funds: mainnet has
        // real collateral and no faucet, so `fund()` must consult both.
        assert!(!Network::Mainnet.has_faucet());
        assert!(Network::Testnet.has_faucet());
        assert!(Network::Local.has_faucet());
    }

    /// `Unknown` must never satisfy a real-funds guard. This is the whole reason
    /// [`Funds`] is a tri-state, and the reason guards ask `is_known_play()`
    /// instead of negating the real case — `!is_real()` would answer `true` here.
    #[test]
    fn unknown_funds_are_not_treated_as_play_funds() {
        assert!(Funds::Play.is_known_play());
        assert!(!Funds::Real.is_known_play());
        assert!(!Funds::Unknown.is_known_play());
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
            let domain = network
                .signing_domain()
                .expect("built-in networks publish name/version");
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
        assert_eq!(default.network(), &Network::Testnet);
        assert_eq!(default.network().funds(), Funds::Play);
    }

    /// Every config carries a `Network`. A raw base URL is a `Custom` target
    /// whose funds are **unknown** — which is what `fund()` refuses on, and it
    /// must refuse for that reason rather than because the network is absent.
    #[test]
    fn config_always_carries_a_network_and_a_raw_base_url_is_custom_unknown() {
        assert_eq!(Config::new(Network::Testnet).network(), &Network::Testnet);

        let raw = Config::with_base_url("http://x");
        assert_eq!(raw.network().funds(), Funds::Unknown);
        assert_eq!(raw.network().label(), "custom");
        assert!(!raw.network().has_faucet());
        assert_eq!(raw.network().ws_base(), None);
        assert_eq!(raw.network().signing_domain(), None);
        assert!(matches!(raw.network(), Network::Custom(_)));
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

        // Trailing slash trimmed on BOTH bases, so joining a path never doubles
        // the separator. `//orders` is a different path than the `/orders` the
        // client signs, so a doubled separator fails verification rather than
        // just looking untidy — pin both halves.
        let slashed = Config::with_base_url("https://preview.example/api/exchange/");
        assert_eq!(slashed.base_url(), "https://preview.example/api/exchange");
        assert_eq!(
            slashed.direct_base_url(),
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

    /// A helper for the `Custom` tests: a syntactically valid target on a host
    /// that provably is not ours. `.invalid` is reserved by RFC 2606 and can
    /// never resolve, so no test here can accidentally address a real deployment.
    fn custom(funds: Funds) -> CustomNetwork {
        CustomNetwork::new("example", "https://example.invalid/api/exchange", funds)
            .expect("valid custom network")
    }

    /// A `Custom` target drives both bases, and the direct `/api/v1` base
    /// defaults to the REST base — because on every deployment that exists today
    /// `/api/v1` is mounted *under* the gateway prefix (ENG-10063). A caller that
    /// genuinely splits them can say so, and then only that base moves.
    #[test]
    fn custom_network_drives_both_bases() {
        let config = Config::new(Network::Custom(custom(Funds::Play)));
        assert_eq!(config.base_url(), "https://example.invalid/api/exchange");
        assert_eq!(
            config.direct_base_url(),
            "https://example.invalid/api/exchange"
        );

        let split = custom(Funds::Play)
            .with_direct_base_url("https://direct.example.invalid")
            .expect("valid direct base");
        let config = Config::new(Network::Custom(split));
        assert_eq!(config.base_url(), "https://example.invalid/api/exchange");
        assert_eq!(config.direct_base_url(), "https://direct.example.invalid");

        // A trailing slash is trimmed on both, so `base + path` never doubles the
        // separator into a path that differs from the one signed.
        let slashed =
            CustomNetwork::new("dev", "https://example.invalid/api/exchange/", Funds::Play)
                .expect("valid custom network");
        assert_eq!(slashed.base_url, "https://example.invalid/api/exchange");
        assert_eq!(
            slashed.direct_base_url,
            "https://example.invalid/api/exchange"
        );
    }

    /// The WS origin is **never derived** from the REST base: it is a separate
    /// host, so an undeclared one stays `None` and the streaming client refuses
    /// to connect rather than guessing. Declared, it is carried verbatim.
    #[test]
    fn custom_ws_url_is_declared_never_derived() {
        // Undeclared: no origin, and nothing invented from the REST host.
        let config = Config::new(Network::Custom(custom(Funds::Play)));
        assert_eq!(config.ws_url(), None);

        let with_ws = custom(Funds::Play)
            .with_ws_url("wss://stream.example.invalid/ws")
            .expect("valid ws url");
        let config = Config::new(Network::Custom(with_ws));
        assert_eq!(config.ws_url(), Some("wss://stream.example.invalid/ws"));
    }

    /// `Custom` must not become the hole in the never-guess-a-signing-domain
    /// rule. Undeclared means `None` — refuse to sign — not a fallback to the
    /// built-in constants, because a signature under the wrong domain can be
    /// valid on a *different* network.
    #[test]
    fn custom_refuses_to_supply_a_signing_domain_it_was_not_given() {
        assert_eq!(
            Network::Custom(custom(Funds::Play)).signing_domain(),
            None,
            "an undeclared domain must be absent, never defaulted"
        );

        let declared = custom(Funds::Play).with_signing_domain(SigningDomain::new(31_337));
        let domain = Network::Custom(declared)
            .signing_domain()
            .expect("declared domain is reported");
        assert_eq!(domain.chain_id, Some(31_337));
        // Name and version still come from the signer's own constants, so the
        // value we advertise and the value that signs cannot drift apart.
        assert_eq!(domain.name, "Nexus Exchange");
        assert_eq!(domain.version, "1");
    }

    /// No hostname of ours is baked in for `Custom`, in either direction: what
    /// the caller passes is what resolves, and nothing from the built-in map
    /// leaks into it. This is the property that keeps unpublished hosts out of
    /// this public artifact — the whole point of the variant.
    #[test]
    fn custom_hardcodes_no_hostname() {
        let network = Network::Custom(custom(Funds::Real));
        for url in [network.base_url(), network.direct_base_url()] {
            assert!(
                url.starts_with("https://example.invalid"),
                "custom must resolve to exactly the caller's host, got {url}"
            );
            assert!(
                !url.contains("nexus"),
                "no built-in host may leak into a custom target, got {url}"
            );
        }
        // And a custom target never collides with a built-in one.
        assert_ne!(network.base_url(), Network::Testnet.base_url());
        assert_ne!(network.base_url(), Network::Mainnet.base_url());
    }

    /// `funds` is caller-declared with no default, and a real-funds `Custom` says
    /// so. The faucet is assumed **absent** until declared, so `fund()` can never
    /// route to one that is not there.
    #[test]
    fn custom_funds_are_declared_and_the_faucet_is_opt_in() {
        assert_eq!(Network::Custom(custom(Funds::Real)).funds(), Funds::Real);
        assert_eq!(Network::Custom(custom(Funds::Play)).funds(), Funds::Play);
        assert_eq!(
            Network::Custom(custom(Funds::Unknown)).funds(),
            Funds::Unknown
        );

        assert!(!Network::Custom(custom(Funds::Play)).has_faucet());
        assert!(Network::Custom(custom(Funds::Play).with_faucet(true)).has_faucet());
    }

    /// `RESERVED_LABELS` is a hand-written list, so nothing but this test stops a
    /// network added later from being claimable as a `Custom` label — which would
    /// let it address that network's stored credentials. Walks the built-in
    /// variants through `label()` (the name storage is keyed on) and requires each
    /// to be listed. A new variant makes this fail rather than open the hole.
    #[test]
    fn reserved_labels_cover_every_built_in_network() {
        for built_in in [Network::Mainnet, Network::Testnet, Network::Local] {
            assert!(
                RESERVED_LABELS.contains(&built_in.label()),
                "built-in network {:?} is missing from RESERVED_LABELS, so a custom \
                 target could claim its credential-storage key",
                built_in.label()
            );
        }
        // The legacy bare-URL target is keyed the same way, so its label is
        // reserved too — asserted against the constant both sites share.
        assert!(RESERVED_LABELS.contains(&LEGACY_BASE_URL_LABEL));
        assert_eq!(
            Network::Custom(CustomNetwork::from_legacy_base_url(
                "https://example.invalid".to_string()
            ))
            .label(),
            LEGACY_BASE_URL_LABEL
        );
    }

    /// The label is required and constrained, because the CLI namespaces stored
    /// **credentials** by it: a label containing a separator or traversal could
    /// make one target's label address another target's credentials.
    #[test]
    fn custom_rejects_labels_that_are_unsafe_as_credential_keys() {
        let ok = "https://example.invalid";
        for bad in [
            "",            // nothing to key on
            "   ",         // whitespace-only, empty once trimmed
            ".",           // a directory, not a network
            "..",          // parent directory — traversal
            "../other",    // traversal into another target's keys
            "one/two",     // path separator
            "one\\two",    // Windows separator
            "one two",     // whitespace
            "one:two",     // namespace separator
            "one\ntwo",    // control character; could split a log line
            "one\u{0}two", // NUL
            "présente",    // non-ASCII: normalization makes keys ambiguous
            // A built-in network's own name: legal under the character set, but
            // it is the key that network's credentials are stored under, so a
            // custom target answering it would address them. Case-insensitively,
            // since a keyring or filesystem key need not be case-sensitive.
            "mainnet",
            "MAINNET",
            "Testnet",
            "local",
            // The label `Config::with_base_url` targets carry, for the same reason.
            "custom",
        ] {
            assert!(
                CustomNetwork::new(bad, ok, Funds::Play).is_err(),
                "label {bad:?} must be rejected"
            );
        }
        // Asserted through `Network::label()` rather than only on the literals
        // above, so the rejection is pinned to the name the accessor actually
        // answers — which is the name credentials would be stored under.
        for built_in in [Network::Mainnet, Network::Testnet, Network::Local] {
            assert!(
                CustomNetwork::new(built_in.label(), ok, Funds::Play).is_err(),
                "label {:?} must be rejected",
                built_in.label()
            );
        }
        // Names that merely *contain* a reserved one stay usable: the collision is
        // an exact key match, and over-rejecting would make plausible stage names
        // ("mainnet-shadow") unavailable for no safety gain.
        for good in ["mainnet-shadow", "pre-testnet", "local2", "customer"] {
            assert!(
                CustomNetwork::new(good, ok, Funds::Play).is_ok(),
                "label {good:?} must be accepted"
            );
        }
        // Over-long labels are refused rather than silently truncated into a
        // *different* key than the caller asked for.
        assert!(CustomNetwork::new("x".repeat(65), ok, Funds::Play).is_err());
        assert!(CustomNetwork::new("x".repeat(64), ok, Funds::Play).is_ok());

        // Ordinary names pass — each accepted character class, and a short one —
        // and surrounding space is trimmed.
        for good in ["dev", "one-two", "one_two", "one.two", "s1"] {
            assert_eq!(
                CustomNetwork::new(good, ok, Funds::Play)
                    .expect("valid label")
                    .label(),
                good
            );
        }
        assert_eq!(
            CustomNetwork::new("  dev  ", ok, Funds::Play)
                .expect("valid label")
                .label(),
            "dev"
        );
    }

    /// Every URL rejection is a request that would otherwise be built *wrong*
    /// rather than merely fail — see `validate_url` for why each one is refused
    /// instead of sanitized.
    #[test]
    fn custom_rejects_urls_that_would_build_a_wrong_request() {
        for bad in [
            "",                                     // no scheme, no host
            "example.invalid",                      // scheme-less
            "//example.invalid",                    // protocol-relative
            "https://",                             // no host
            "https://:8080",                        // a port is not a host
            "https://:8080/api",                    // ...nor with a path after it
            "file:///etc/passwd",                   // not a network target
            "data:text/plain,x",                    // not a network target
            "javascript:alert(1)",                  // not a network target
            "ftp://example.invalid",                // unsupported scheme
            "ws://example.invalid",                 // a WS origin is not a REST base
            "https://user:pw@example.invalid",      // userinfo leaks into logs
            "https://example.invalid?x=1",          // query swallows the path
            "https://example.invalid/api#frag",     // fragment swallows the path
            "https://stage .invalid",               // whitespace
            "https://example.invalid/\nHost: evil", // header injection
        ] {
            assert!(
                CustomNetwork::new("dev", bad, Funds::Play).is_err(),
                "base URL {bad:?} must be rejected"
            );
        }

        // The direct base is held to the same standard as the REST base...
        assert!(custom(Funds::Play)
            .with_direct_base_url("https://user:pw@example.invalid")
            .is_err());

        // ...and the WS origin to the WS schemes. A REST base is not a WS origin.
        for bad in [
            "https://stream.example.invalid",
            "stream.example.invalid",
            "wss://user:pw@stream.example.invalid",
            "wss://stream.example.invalid?token=leaked",
        ] {
            assert!(
                custom(Funds::Play).with_ws_url(bad).is_err(),
                "ws URL {bad:?} must be rejected"
            );
        }
        for good in ["ws://localhost:9090/ws", "wss://stream.example.invalid/ws"] {
            assert!(custom(Funds::Play).with_ws_url(good).is_ok());
        }
    }

    /// A rejected URL or label must leave nothing behind. `CustomNetwork::new`
    /// returns `Result`, so there is no half-built target to accidentally use —
    /// this pins that the error path yields no value at all.
    #[test]
    fn a_rejected_custom_network_yields_no_config() {
        let err = CustomNetwork::new("dev", "file:///etc/passwd", Funds::Play)
            .expect_err("rejected scheme");
        assert!(err.to_string().contains("http"), "got: {err}");
    }

    /// The config types stay `Send + Sync` now that `Network` carries owned data,
    /// so a `Client` is still shareable across tasks and threads.
    ///
    /// `CustomNetwork` is plain immutable data — no interior mutability, no locks,
    /// nothing to contend on — so there is no ordering to get wrong and no
    /// deadlock to reach. This is a compile-time assertion; it fails to build,
    /// not to run, if that ever stops being true.
    #[test]
    fn config_types_stay_send_and_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Funds>();
        assert_send_sync::<CustomNetwork>();
        assert_send_sync::<Network>();
        assert_send_sync::<Config>();
        assert_send_sync::<crate::Client>();
    }
}
