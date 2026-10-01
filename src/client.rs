//! The HTTP client — entry point for the SDK.

use std::sync::Arc;
use std::time::Duration;

use backon::BackoffBuilder;
use serde::{de::DeserializeOwned, Serialize};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::auth::{Credential, SigningContext};
use crate::config::{API_VERSION_HEADER, API_VERSION_RAW, DEFAULT_USER_AGENT};
use crate::ratelimit::{RateLimiter, ThrottleInfo};
use crate::rest::pagination::{Cursor, NEXT_CURSOR_HEADER};
use crate::types::RateLimitStatus;
use crate::{Config, Error, Network, Result};

/// The `{ code, message }` error envelope returned by the API on failures.
#[derive(serde::Deserialize)]
struct ApiErrorBody {
    code: String,
    message: Option<String>,
}

/// Why a [`Network::Mainnet`] client refuses every request. Kept as one
/// constant so the REST path and any future caller give the identical reason.
pub(crate) const MAINNET_NOT_TARGETABLE: &str =
    "Network::Mainnet is not targetable by this release: \
     api.nexus.xyz does not resolve yet (ENG-8155). Requests are refused locally rather than \
     sent to a real-funds host on a guessed URL. Use Network::Testnet, or Network::Custom to \
     target a host you control — including a real-funds one, where you supply the URL and so own \
     its path layout.";

/// Build the underlying HTTP client with the configured `User-Agent`.
///
/// The UA is already normalized to a valid header value in
/// [`Config::with_user_agent`](crate::Config::with_user_agent), so the first
/// build should succeed; the fall back to the always-valid
/// [`DEFAULT_USER_AGENT`] is defense-in-depth against a malformed UA reaching
/// here some other way, so we never panic or drop attribution silently. The
/// final `expect` only fires on a genuine TLS/resolver init failure — the same
/// condition under which [`reqwest::Client::new`] itself panics — so this keeps
/// [`Client::new`] infallible without hiding that class of error.
fn build_http(user_agent: &str) -> reqwest::Client {
    let default_headers = default_headers();
    reqwest::Client::builder()
        .user_agent(user_agent)
        .default_headers(default_headers.clone())
        .build()
        .or_else(|_| {
            reqwest::Client::builder()
                .user_agent(DEFAULT_USER_AGENT)
                .default_headers(default_headers)
                .build()
        })
        .expect("failed to initialize HTTP client (TLS/resolver init)")
}

/// Headers sent by default on every REST request. Currently just the pinned
/// spec tag ([`API_VERSION_HEADER`]: [`API_VERSION_RAW`], trimmed), which lets
/// the server-side request indexer meter edge usage per spec version
/// (ENG-4804). The tag is wired in from existing pinned state — no new
/// [`Config`] field. A `vX.Y.Z` tag is always a valid header value, but if
/// parsing ever failed we skip the header rather than panic during client init.
fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(API_VERSION_HEADER.as_bytes()),
        HeaderValue::from_str(API_VERSION_RAW.trim()),
    ) {
        headers.insert(name, value);
    }
    headers
}

/// Entry point for the Nexus Exchange API.
///
/// Construct with [`Client::new`]. REST methods live in [`crate::rest`];
/// streaming in [`crate::ws`].
///
/// The client paces itself against the server's rate limit: it honors `429` +
/// `Retry-After` (retrying up to [`RateLimit::max_retries`](crate::RateLimit::max_retries))
/// and, when enabled, proactively meters requests through a cost-weighted token
/// bucket. Call [`Client::fetch_rate_limit_status`] to sync that bucket to the
/// caller's live server-side budget.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    pub(crate) config: Config,
    limiter: Arc<RateLimiter>,
}

impl Client {
    /// Create a client for the given [`Config`].
    pub fn new(config: Config) -> Self {
        let limiter = Arc::new(RateLimiter::new(&config.rate_limit));
        Self {
            http: build_http(&config.user_agent),
            config,
            limiter,
        }
    }

    /// The configured base URL.
    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// The REST base every request is appended to.
    ///
    /// There is one base: the path a method names is the path it signs, and the
    /// base's own prefix (`/v1` on testnet) is stripped at the edge before the
    /// indexer verifies (EDR-006, ENG-18324).
    ///
    /// # Why this returns `Result`
    ///
    /// This is also the single gate that refuses [`Network::Mainnet`]. Every
    /// request builder in this module resolves its base here, so returning
    /// `Result` makes the real-funds check **impossible to omit**: a new helper
    /// that forgets it does not compile. A boolean checked at each call site
    /// would be one forgotten `if` away from putting a wrongly-built,
    /// wrongly-signed request on a real-money host.
    ///
    /// The rejection is local and total — it happens before any DNS, TLS or
    /// bytes on the wire, and before any credential is used.
    fn base(&self) -> Result<&str> {
        // Keyed on the `Mainnet` *variant*, not on `funds()`. The refusal is
        // about a host that does not resolve yet, not about real money, so a
        // `Network::Custom` declaring `Funds::Real` is targetable — the caller
        // supplied a URL that already resolves. Money movement is guarded
        // separately, in `Client::fund`.
        if matches!(self.config.network, Network::Mainnet) {
            return Err(Error::invalid_request(MAINNET_NOT_TARGETABLE));
        }
        Ok(&self.config.base_url)
    }

    /// The WebSocket URL to open, or why there is none.
    ///
    /// The streaming counterpart of [`base`](Self::base), and the gate
    /// every streaming entry point resolves its URL through: it refuses
    /// [`Network::Mainnet`] for the same reason REST does — `api.nexus.xyz`
    /// does not resolve yet, so [`Network::ws_base`] reports the shape of a
    /// host that is not there — and reports a network that declares no socket
    /// rather than guessing one.
    pub(crate) fn ws_endpoint(&self) -> Result<&str> {
        if matches!(self.config.network, Network::Mainnet) {
            return Err(Error::invalid_request(MAINNET_NOT_TARGETABLE));
        }
        self.config.ws_url.as_deref().ok_or_else(|| {
            Error::invalid_request(
                "no WebSocket endpoint configured for this network; set one with \
                 Config::with_ws_url",
            )
        })
    }

    /// Unauthenticated `GET`, deserializing the JSON response and decoding the
    /// API's `{ code, message }` envelope on non-2xx.
    ///
    /// `cost` is the endpoint's rate-limit weight: it is reserved from the token
    /// bucket before the request goes out (0 for endpoints the server does not
    /// charge). On `429` the request is retried, honoring `Retry-After`, up to
    /// the configured retry ceiling.
    ///
    /// Each attempt is bounded by [`Config::with_timeout`]. Transient transport
    /// failures (connect/timeout) and `5xx`/`408` responses are retried with
    /// exponential backoff per [`Config::with_retry`]; `429` stays owned by the
    /// rate-limit path above so the two layers never double-retry it. **Retry
    /// is only safe because this is a `GET`** — non-idempotent methods must not
    /// reuse this path (a lost-response retry would double-submit); see the
    /// signed helpers, which time out per attempt but do not auto-retry.
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cost: f64,
    ) -> Result<T> {
        Ok(self.get_page(path, query, cost).await?.0)
    }

    /// Unauthenticated `GET` on a cursor-paginated list endpoint: the decoded
    /// body **and** the [`Cursor`] from the `X-Next-Cursor` response header.
    ///
    /// Identical to [`get`](Self::get) — same rate-limit accounting, retry and
    /// error decoding — except that the pagination cursor survives. It has to be
    /// read here: the response body of a paginated list endpoint is a bare array,
    /// so the only place the next page is advertised is a header, which
    /// [`handle`](Self::handle) would otherwise drop.
    ///
    /// `None` for the cursor means the server sent no `X-Next-Cursor`, i.e. this
    /// was the last page. That is the documented end-of-results signal, not a
    /// failure.
    pub(crate) async fn get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cost: f64,
    ) -> Result<(T, Option<Cursor>)> {
        let url = format!("{}{}", self.base()?, path);

        // Reserve the endpoint's cost once for this logical request. Retries
        // below reuse that reservation and pace off `Retry-After` instead, so a
        // request that needs N attempts is still charged the bucket only once —
        // matching how the server accounts for it.
        let wait = self.limiter.reserve(cost);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }

        let mut attempt: u32 = 0;
        // Backoff for transient transport / 5xx / 408 failures on this
        // idempotent GET. Independent of the 429 path below, which the rate
        // limiter owns end-to-end.
        let mut transient = self.config.retry.backoff().build();
        loop {
            let resp = match self
                .http
                .get(&url)
                .query(query)
                .timeout(self.config.timeout)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    // A transport failure at the send site (connect/DNS/TLS or
                    // a per-attempt timeout) is transient; classify it through
                    // the taxonomy and retry until backoff is exhausted, then
                    // surface the error.
                    let err = Error::from(e);
                    if err.is_retryable() {
                        if let Some(delay) = transient.next() {
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    }
                    return Err(err);
                }
            };

            if resp.status().as_u16() == 429 {
                let info = ThrottleInfo::from_headers(resp.headers());
                self.limiter.note_throttle(&info);
                if attempt < self.limiter.max_retries() {
                    attempt += 1;
                    // Fall back to capped exponential back-off only when the
                    // server gives us no Retry-After / reset hint.
                    let backoff =
                        Duration::from_millis(250u64.saturating_mul(1u64 << attempt.min(6)));
                    tokio::time::sleep(info.wait(backoff)).await;
                    continue;
                }
                return Err(crate::error::TransientError::RateLimited {
                    retry_after: info.retry_after,
                }
                .into());
            }

            // A 5xx / 408 response is transient: retry per the backoff before
            // giving up. Success and terminal errors return unchanged. The 429
            // path above already returned, so `handle` never yields a
            // `RateLimited` here — the rate-limit layer stays the sole owner of
            // 429 and the two layers never double-retry it.
            match self.handle_page(resp).await {
                Err(err) if err.is_retryable() => {
                    if let Some(delay) = transient.next() {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(err);
                }
                other => return other,
            }
        }
    }

    /// Unauthenticated `POST` with a JSON body — used by the wallet-signed auth
    /// flows (`/auth/login`, `/agents/register`), where authorization travels
    /// in the request body rather than HMAC headers.
    ///
    /// Not auto-retried: a `POST` is non-idempotent, so replaying it after a
    /// lost response could double-submit. Each attempt is still bounded by
    /// [`Config::with_timeout`]. No credentials are attached and the rate-limit
    /// bucket is not charged — these are bootstrap calls made before the caller
    /// holds a key.
    pub(crate) async fn post_unsigned<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let body_bytes = serde_json::to_vec(body)?;
        let req = self
            .http
            .post(format!("{}{}", self.base()?, path))
            .timeout(self.config.timeout)
            .header("content-type", "application/json")
            .body(body_bytes);
        self.handle(req.send().await?).await
    }

    /// Signed `GET` — signs the exact path + query string that is sent.
    pub(crate) async fn signed_get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        Ok(self.signed_get_page(path, query).await?.0)
    }

    /// Signed `GET` on a cursor-paginated list endpoint: the decoded body **and**
    /// the [`Cursor`] from the `X-Next-Cursor` response header. The signed-route
    /// counterpart of [`get_page`](Self::get_page).
    ///
    /// The cursor rides in the query string, so it is covered by the signature on
    /// every page — each page of a walk is independently signed over the exact
    /// path and query sent.
    pub(crate) async fn signed_get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(T, Option<Cursor>)> {
        // Resolve the base *before* signing: a refused network must not consume
        // a nonce or produce a signature for a request that will never be sent.
        let base = self.base()?;
        let creds = self.creds()?;
        let qs = serde_urlencoded::to_string(query).unwrap_or_default();
        let headers = creds.auth_headers(&SigningContext {
            method: "GET",
            path,
            query: &qs,
            body: b"",
            timestamp_ms: self.nonce(),
        })?;
        let url = if qs.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{qs}")
        };
        let mut req = self.http.get(url).timeout(self.config.timeout);
        for (name, value) in &headers {
            req = req.header(*name, value);
        }
        self.handle_page(req.send().await?).await
    }

    /// Signed `POST` with a JSON body.
    pub(crate) async fn signed_post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.signed_with_body(reqwest::Method::POST, path, body)
            .await
    }

    /// Signed `PUT` with a JSON body.
    pub(crate) async fn signed_put<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.signed_with_body(reqwest::Method::PUT, path, body)
            .await
    }

    /// Signed `DELETE` (no body, no query).
    pub(crate) async fn signed_delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.signed_no_body(reqwest::Method::DELETE, path, &[])
            .await
    }

    /// Signed `DELETE` carrying a query string (e.g. a market-scoped cancel).
    /// Signs the exact path + query that is sent, exactly like [`signed_get`].
    pub(crate) async fn signed_delete_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        self.signed_no_body(reqwest::Method::DELETE, path, query)
            .await
    }

    /// Signed `PATCH` carrying BOTH a query string and a JSON body — signs the
    /// exact path + query + body that is sent (e.g.
    /// `PATCH /orders/{id}?market_id=…`, where the query routes the request to
    /// the owning market and the body carries the change). The query is signed
    /// separately from the path, exactly like [`signed_get`].
    pub(crate) async fn signed_patch_with_query<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        body: &B,
    ) -> Result<T> {
        // Propagate an encode failure rather than silently dropping the query:
        // on a by-id route the query carries the required routing key, so a
        // silently empty query would misroute the request.
        let qs = serde_urlencoded::to_string(query)
            .map_err(|e| Error::invalid_request(format!("could not encode query string: {e}")))?;
        // Resolve through `base` like every other builder, and before signing.
        // This one used to read `config.base_url` directly, which opted out of
        // the real-funds gate. One rule, one place — no builder gets its own base.
        let base = self.base()?;
        let body_bytes = serde_json::to_vec(body)?;
        let creds = self.creds()?;
        let _turn = write_turn(creds).await;
        let headers = creds.auth_headers(&SigningContext {
            method: "PATCH",
            path,
            query: &qs,
            body: &body_bytes,
            timestamp_ms: self.nonce(),
        })?;
        let url = if qs.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{qs}")
        };
        let mut req = self
            .http
            .request(reqwest::Method::PATCH, url)
            .timeout(self.config.timeout)
            .header("content-type", "application/json")
            .body(body_bytes);
        for (name, value) in &headers {
            req = req.header(*name, value);
        }
        self.handle(req.send().await?).await
    }

    /// Signed `POST` with no body (e.g. token mint).
    pub(crate) async fn signed_post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.signed_no_body(reqwest::Method::POST, path, &[]).await
    }

    fn creds(&self) -> Result<&dyn crate::auth::Credential> {
        self.config
            .credentials
            .as_deref()
            .ok_or_else(|| Error::credentials("this endpoint requires credentials"))
    }

    /// Next millisecond timestamp/nonce from the configured [`Nonce`] source.
    fn nonce(&self) -> u64 {
        self.config.nonce.next()
    }

    async fn signed_with_body<B: Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &B,
    ) -> Result<T> {
        // Resolve the base *before* signing — see `signed_get_page`.
        let base = self.base()?;
        let body_bytes = serde_json::to_vec(body)?;
        let creds = self.creds()?;
        let _turn = write_turn(creds).await;
        let headers = creds.auth_headers(&SigningContext {
            method: method.as_str(),
            path,
            query: "",
            body: &body_bytes,
            timestamp_ms: self.nonce(),
        })?;
        let mut req = self
            .http
            .request(method, format!("{base}{path}"))
            .timeout(self.config.timeout)
            .header("content-type", "application/json")
            .body(body_bytes);
        for (name, value) in &headers {
            req = req.header(*name, value);
        }
        self.handle(req.send().await?).await
    }

    async fn signed_no_body<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        // Propagate an encode failure rather than collapsing to an empty query:
        // for a scoped DELETE (e.g. `cancel_orders_for_market`) a silently empty
        // query would widen `DELETE /orders?market_id=…` into the account-wide
        // `DELETE /orders`, defeating the very guard the scoped call exists for.
        let qs = serde_urlencoded::to_string(query)
            .map_err(|e| Error::invalid_request(format!("could not encode query string: {e}")))?;
        // Resolve the base *before* signing — see `signed_get_page`.
        let base = self.base()?;
        let creds = self.creds()?;
        let _turn = write_turn(creds).await;
        let headers = creds.auth_headers(&SigningContext {
            method: method.as_str(),
            path,
            query: &qs,
            body: b"",
            timestamp_ms: self.nonce(),
        })?;
        let url = if qs.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{qs}")
        };
        // These methods semantically carry a payload even when that payload is
        // empty. Set `Content-Length: 0` explicitly; reqwest does not emit the
        // header for `body(Vec::new())` here, while strict gateways reject
        // the request before it reaches the API. DELETE remains bodyless.
        let needs_explicit_empty_body = matches!(
            method,
            reqwest::Method::POST | reqwest::Method::PUT | reqwest::Method::PATCH
        );
        let mut req = self.http.request(method, url).timeout(self.config.timeout);
        if needs_explicit_empty_body {
            req = req.header(reqwest::header::CONTENT_LENGTH, "0");
        }
        for (name, value) in &headers {
            req = req.header(*name, value);
        }
        self.handle(req.send().await?).await
    }

    /// Decode a response, mapping the `{ code, message }` envelope on non-2xx.
    async fn handle<T: DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        Ok(self.handle_page(resp).await?.0)
    }

    /// [`handle`](Self::handle), also returning the `X-Next-Cursor` cursor.
    ///
    /// The single decode path for every response, so a paginated endpoint gets
    /// exactly the same error mapping as any other; non-paginated callers go
    /// through [`handle`](Self::handle) and drop the (always-absent) cursor.
    async fn handle_page<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<(T, Option<Cursor>)> {
        let status = resp.status();
        // Read the header hints before the response is consumed by `bytes()`.
        let retry_after = parse_retry_after(resp.headers());
        let next_cursor = parse_next_cursor(resp.headers());
        let bytes = resp.bytes().await?;
        if status.is_success() {
            return Ok((serde_json::from_slice(&bytes)?, next_cursor));
        }
        // Decode the `{ code, message }` envelope; fall back to the status when
        // the body isn't the expected shape. `Error::from_api` classifies into
        // the terminal/transient trees.
        let (code, message) = match serde_json::from_slice::<ApiErrorBody>(&bytes) {
            Ok(env) => (env.code, env.message.unwrap_or_default()),
            Err(_) => (
                status.as_str().to_string(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ),
        };
        Err(Error::from_api(status, retry_after, code, message))
    }

    /// Sync the client-side limiter to a server-reported rate-limit snapshot.
    pub(crate) fn sync_rate_limit(&self, status: &RateLimitStatus) {
        self.limiter.sync(status.limit, status.remaining);
    }
}

/// Wait for a mutating request's turn in the credential's
/// [`write_queue`](crate::Credential::write_queue), if it has one.
///
/// Every signed write takes this before it is signed and holds the guard until
/// its response is decoded, so the nonce is drawn in send order and the next
/// write cannot overtake it on the wire. Signed reads skip it: the server
/// records no nonce on a `GET`.
async fn write_turn(creds: &dyn Credential) -> Option<tokio::sync::MutexGuard<'_, ()>> {
    match creds.write_queue() {
        Some(queue) => Some(queue.turn().await),
        None => None,
    }
}

/// Upper bound on a server-advised `Retry-After`. A buggy or hostile gateway
/// could send an absurd value (`Retry-After: 99999999999`); without a cap a
/// retry layer honoring [`crate::Error::retry_after`] would sleep effectively
/// forever. Five minutes is well beyond any legitimate rate-limit window.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Read the next-page [`Cursor`] out of a response's `X-Next-Cursor` header.
///
/// `None` when the header is absent — the spec's signal that this was the last
/// page — and also when it is present but blank or unreadable. A blank cursor
/// cannot be sent back meaningfully: passing one on as `cursor=` would re-request
/// the first page forever, so it is treated as absent (which terminates the walk)
/// rather than as a cursor.
fn parse_next_cursor(headers: &reqwest::header::HeaderMap) -> Option<Cursor> {
    let raw = headers.get(NEXT_CURSOR_HEADER)?.to_str().ok()?.trim();
    (!raw.is_empty()).then(|| Cursor::new(raw))
}

/// Parse a `Retry-After` header expressed in seconds (the form the gateway
/// emits), clamped to [`MAX_RETRY_AFTER`]. HTTP-date forms are ignored (treated
/// as absent).
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use wiremock::matchers::{header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn retry_after(value: &str) -> Option<Duration> {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_str(value).unwrap());
        parse_retry_after(&headers)
    }

    #[test]
    fn retry_after_parses_seconds() {
        assert_eq!(retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(retry_after(" 12 "), Some(Duration::from_secs(12)));
    }

    #[test]
    fn retry_after_clamps_unbounded_values() {
        // A hostile/buggy gateway can't make a retry layer sleep forever.
        assert_eq!(retry_after("99999999999"), Some(MAX_RETRY_AFTER));
        assert_eq!(retry_after("301"), Some(MAX_RETRY_AFTER));
        assert_eq!(retry_after("300"), Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn retry_after_ignores_non_numeric_and_dates() {
        assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(retry_after("garbage"), None);
    }

    /// The SDK sends its descriptive default `User-Agent` so the server can
    /// attribute traffic to the Rust SDK rather than reqwest's generic default.
    #[tokio::test]
    // These tests point at a wiremock origin, which is exactly the throwaway
    // target the deprecated selector still serves. Silenced here, never for
    // callers.
    #[allow(deprecated)]
    async fn sends_default_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header("user-agent", DEFAULT_USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(Config::with_base_url(server.uri()));
        let _: serde_json::Value = client.get("/x", &[], 0.0).await.unwrap();
    }

    /// Every request carries the pinned spec tag as `X-Nexus-Api-Version`,
    /// sourced from the repo-root `.api-version` file, so the server can meter
    /// edge usage per spec version (ENG-4804). Capture it off a normal request.
    #[tokio::test]
    #[allow(deprecated)] // Throwaway wiremock target; see above.
    async fn sends_api_version_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header("x-nexus-api-version", API_VERSION_RAW.trim()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(Config::with_base_url(server.uri()));
        let _: serde_json::Value = client.get("/x", &[], 0.0).await.unwrap();
    }

    /// An embedding application (CLI, web frontend) can override the UA to
    /// identify itself — this is what unlocks the per-client breakdown.
    #[tokio::test]
    #[allow(deprecated)] // Throwaway wiremock target; see above.
    async fn sends_overridden_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header("user-agent", "nexus-cli/1.2.3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            Client::new(Config::with_base_url(server.uri()).with_user_agent("nexus-cli/1.2.3"));
        let _: serde_json::Value = client.get("/x", &[], 0.0).await.unwrap();
    }

    /// A UA with bytes illegal in an HTTP header must not panic construction;
    /// the client falls back to the always-valid default UA instead.
    #[tokio::test]
    #[allow(deprecated)] // Throwaway wiremock target; see above.
    async fn invalid_user_agent_falls_back_to_default() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header("user-agent", DEFAULT_USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        // A newline is not a legal header-value byte.
        let client = Client::new(Config::with_base_url(server.uri()).with_user_agent("bad\nua"));
        let _: serde_json::Value = client.get("/x", &[], 0.0).await.unwrap();
    }

    /// Every path goes to the one REST base, which on testnet is the published
    /// `/v1` (EDR-006, ENG-18324).
    #[test]
    fn testnet_requests_go_to_the_v1_base() {
        let client = Client::new(Config::new(Network::Testnet));
        assert_eq!(client.base().unwrap(), "https://api.testnet.nexus.xyz/v1");
    }

    /// A `Mainnet` client resolves **no** base. This is the
    /// choke point every request builder goes through, so proving it here proves
    /// no request of any kind can reach a real-funds host.
    #[test]
    #[allow(deprecated)] // Throwaway bare-URL target; see above.
    fn mainnet_resolves_no_base() {
        let client = Client::new(Config::new(Network::Mainnet));
        let err = client.base().expect_err("mainnet must not resolve a base");
        assert!(
            !err.is_retryable(),
            "the refusal is a permanent local decision, not a transient failure"
        );
        // The play-funds networks are unaffected by the guard.
        assert!(Client::new(Config::new(Network::Testnet)).base().is_ok());
        assert!(Client::new(Config::new(Network::Local)).base().is_ok());
        // A custom base URL carries no network, so it is never gated here.
        assert!(Client::new(Config::with_base_url("http://127.0.0.1:1"))
            .base()
            .is_ok());
    }

    /// The real-funds refusal must come *before* the request is signed: no
    /// signature computed, and no nonce drawn. A stateless `SystemTimeNonce`
    /// wouldn't care, but a caller-supplied monotonic counter would silently
    /// desync against the server if a never-sent request consumed a value.
    #[tokio::test]
    async fn mainnet_refusal_consumes_no_nonce_and_no_signature() {
        use crate::auth::Nonce;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug, Default)]
        struct CountingNonce(AtomicUsize);
        impl Nonce for CountingNonce {
            fn next(&self) -> u64 {
                self.0.fetch_add(1, Ordering::SeqCst) as u64 + 1
            }
        }

        let nonce = Arc::new(CountingNonce::default());
        let client = Client::new(
            Config::new(Network::Mainnet)
                .api_key(
                    "nx_test",
                    "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
                )
                .with_nonce(nonce.clone()),
        );

        // One representative of each signed builder shape.
        let signed_get: Result<serde_json::Value> = client.signed_get("/account", &[]).await;
        assert!(signed_get.is_err());
        let signed_post: Result<serde_json::Value> =
            client.signed_post("/orders", &serde_json::json!({})).await;
        assert!(signed_post.is_err());
        let signed_delete: Result<serde_json::Value> = client.signed_delete("/orders/o1").await;
        assert!(signed_delete.is_err());
        let patched: Result<serde_json::Value> = client
            .signed_patch_with_query(
                "/orders/o1",
                &[("market_id", "BTC-USDX-PERP".to_string())],
                &serde_json::json!({}),
            )
            .await;
        assert!(patched.is_err());

        assert_eq!(
            nonce.0.load(Ordering::SeqCst),
            0,
            "a refused request must not draw a nonce"
        );
    }

    /// A signed request goes out under the base's own prefix (`/v1/account`)
    /// but is signed over the bare path (`/account`): the edge strips `/v1`
    /// before the indexer verifies, so the signature must cover the path the
    /// indexer receives. Pinned by recomputing the expected signature.
    #[tokio::test]
    #[allow(deprecated)] // Throwaway wiremock target; see above.
    async fn prefixed_base_sends_under_prefix_and_signs_the_bare_path() {
        use crate::auth::{Credential, Credentials};

        const SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        #[derive(Debug)]
        struct FixedNonce;
        impl crate::auth::Nonce for FixedNonce {
            fn next(&self) -> u64 {
                1_700_000_000_000
            }
        }
        let expected = Credentials::api_key("nx", SECRET)
            .auth_headers(&SigningContext {
                method: "GET",
                path: "/account",
                query: "",
                body: &[],
                timestamp_ms: 1_700_000_000_000,
            })
            .unwrap()
            .into_iter()
            .find(|(name, _)| *name == "x-signature")
            .unwrap()
            .1;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/account"))
            .and(header("x-signature", expected.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(
            Config::with_base_url(format!("{}/v1", server.uri()))
                .api_key("nx", SECRET)
                .with_nonce(Arc::new(FixedNonce)),
        );
        let _: serde_json::Value = client.signed_get("/account", &[]).await.unwrap();
    }

    #[tokio::test]
    #[allow(deprecated)] // Throwaway wiremock target; see above.
    async fn signed_get_signs_and_sends_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(query_param("limit", "10"))
            .and(header_exists("x-signature"))
            .and(header_exists("x-timestamp"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        let client = Client::new(Config::with_base_url(server.uri()).api_key(
            "nx",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        ));
        let _: serde_json::Value = client
            .signed_get("/x", &[("limit", "10".to_string())])
            .await
            .unwrap();
    }

    /// ENG-17010: while an agent signer's write queue is held, a signed read
    /// still goes straight out (the server records no nonce on a `GET`), and a
    /// signed write waits its turn.
    #[tokio::test]
    #[allow(deprecated)] // Throwaway wiremock target; see above.
    async fn agent_writes_queue_but_reads_do_not() {
        let server = MockServer::start().await;
        Mock::given(path("/x"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let signer = Arc::new(
            crate::AgentSigner::from_hex(
                "0x0101010101010101010101010101010101010101010101010101010101010101",
            )
            .unwrap(),
        );
        let client =
            Client::new(Config::with_base_url(server.uri()).with_credential(signer.clone()));

        let held = signer.write_queue().unwrap().turn().await;
        let wait = Duration::from_millis(500);
        let read = tokio::time::timeout(wait, client.signed_get::<serde_json::Value>("/x", &[]));
        assert!(
            read.await.is_ok(),
            "a signed read must not wait on the write queue"
        );
        let body = serde_json::json!({});
        let write = tokio::time::timeout(
            wait,
            client.signed_post::<_, serde_json::Value>("/x", &body),
        );
        assert!(write.await.is_err(), "a signed write must wait its turn");
        drop(held);
        let _: serde_json::Value = client.signed_post("/x", &body).await.unwrap();
    }
}
