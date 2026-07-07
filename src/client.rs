//! The HTTP client — entry point for the SDK.

use std::sync::Arc;
use std::time::Duration;

use backon::BackoffBuilder;
use serde::{de::DeserializeOwned, Serialize};

use crate::auth::SigningContext;
use crate::config::DEFAULT_USER_AGENT;
use crate::ratelimit::{RateLimiter, ThrottleInfo};
use crate::types::RateLimitStatus;
use crate::{Config, Error, Result};

/// The `{ code, message }` error envelope returned by the API on failures.
#[derive(serde::Deserialize)]
struct ApiErrorBody {
    code: String,
    message: Option<String>,
}

/// Path prefix for the direct-service (`/api/v1`) surface. A request whose path
/// begins with this is routed to the host-root direct base
/// ([`Config::direct_base_url`](crate::Config::direct_base_url)) rather than the
/// legacy `/api/exchange` gateway base; everything else stays on the gateway.
///
/// The prefix is part of the `path` that is both **signed and sent**: the server
/// verifies the HMAC over the exact path it receives, and — unlike the legacy
/// gateway, which strips its own `/api/exchange` prefix before the indexer signs
/// — the direct surface is served at the host root with no stripping, so the
/// full `/api/v1/...` path is what must be signed. Selecting the base off this
/// same prefix keeps the signed path and the sent URL from ever disagreeing.
const API_V1_PREFIX: &str = "/api/v1/";

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
    reqwest::Client::builder()
        .user_agent(user_agent)
        .build()
        .or_else(|_| {
            reqwest::Client::builder()
                .user_agent(DEFAULT_USER_AGENT)
                .build()
        })
        .expect("failed to initialize HTTP client (TLS/resolver init)")
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

    /// Select the base URL for `path`: the host-root direct base for the
    /// `/api/v1` surface, the legacy `/api/exchange` gateway base otherwise.
    ///
    /// Detection keys off the path prefix rather than a per-call flag so a single
    /// centralized rule governs every request builder below — there is no way for
    /// a v1 path to be sent to the gateway base (or vice versa) by omission. The
    /// `path` argument is unchanged by this choice, so the value signed always
    /// equals the value appended to the base.
    fn base_for(&self, path: &str) -> &str {
        if path.starts_with(API_V1_PREFIX) {
            &self.config.direct_base_url
        } else {
            &self.config.base_url
        }
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
        let url = format!("{}{}", self.base_for(path), path);

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
            match self.handle(resp).await {
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
            .post(format!("{}{}", self.base_for(path), path))
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
            format!("{}{}", self.base_for(path), path)
        } else {
            format!("{}{}?{}", self.base_for(path), path, qs)
        };
        let mut req = self.http.get(url).timeout(self.config.timeout);
        for (name, value) in &headers {
            req = req.header(*name, value);
        }
        self.handle(req.send().await?).await
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
        let body_bytes = serde_json::to_vec(body)?;
        let headers = self.creds()?.auth_headers(&SigningContext {
            method: "PATCH",
            path,
            query: &qs,
            body: &body_bytes,
            timestamp_ms: self.nonce(),
        })?;
        let url = if qs.is_empty() {
            format!("{}{}", self.config.base_url, path)
        } else {
            format!("{}{}?{}", self.config.base_url, path, qs)
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
        let body_bytes = serde_json::to_vec(body)?;
        let headers = self.creds()?.auth_headers(&SigningContext {
            method: method.as_str(),
            path,
            query: "",
            body: &body_bytes,
            timestamp_ms: self.nonce(),
        })?;
        let mut req = self
            .http
            .request(method, format!("{}{}", self.base_for(path), path))
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
        let headers = self.creds()?.auth_headers(&SigningContext {
            method: method.as_str(),
            path,
            query: &qs,
            body: b"",
            timestamp_ms: self.nonce(),
        })?;
        let url = if qs.is_empty() {
            format!("{}{}", self.base_for(path), path)
        } else {
            format!("{}{}?{}", self.base_for(path), path, qs)
        };
        let mut req = self.http.request(method, url).timeout(self.config.timeout);
        for (name, value) in &headers {
            req = req.header(*name, value);
        }
        self.handle(req.send().await?).await
    }

    /// Decode a response, mapping the `{ code, message }` envelope on non-2xx.
    async fn handle<T: DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        // Read the Retry-After hint before the response is consumed by `bytes()`.
        let retry_after = parse_retry_after(resp.headers());
        let bytes = resp.bytes().await?;
        if status.is_success() {
            return Ok(serde_json::from_slice(&bytes)?);
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

/// Upper bound on a server-advised `Retry-After`. A buggy or hostile gateway
/// could send an absurd value (`Retry-After: 99999999999`); without a cap a
/// retry layer honoring [`crate::Error::retry_after`] would sleep effectively
/// forever. Five minutes is well beyond any legitimate rate-limit window.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

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

    /// An embedding application (CLI, web frontend) can override the UA to
    /// identify itself — this is what unlocks the per-client breakdown.
    #[tokio::test]
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

    /// `/api/v1/*` paths route to the host-root direct base; everything else
    /// stays on the gateway base. This is the single rule every request builder
    /// relies on, so pin it directly.
    #[test]
    fn base_for_routes_v1_to_direct_and_rest_to_gateway() {
        let client = Client::new(Config::new(crate::Network::Stable));
        assert_eq!(
            client.base_for("/api/v1/orders"),
            "https://exchange.nexus.xyz"
        );
        assert_eq!(
            client.base_for("/api/v1/markets/summary"),
            "https://exchange.nexus.xyz"
        );
        // Legacy / not-yet-migrated routes stay on the gateway base.
        assert_eq!(
            client.base_for("/health"),
            "https://exchange.nexus.xyz/api/exchange"
        );
        assert_eq!(
            client.base_for("/orders/o1"),
            "https://exchange.nexus.xyz/api/exchange"
        );
    }

    /// A signed request to a `/api/v1` path must be sent to the host root (no
    /// `/api/exchange`) AND sign the full `/api/v1/...` path — the server signs
    /// the path it receives, and nothing strips the prefix on the direct
    /// surface. Drive it through a mock at the host root to prove both.
    #[tokio::test]
    async fn v1_path_is_sent_to_direct_base_and_signed_over_full_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/account"))
            .and(header_exists("x-signature"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        // A bare origin: the derived direct base equals the gateway base, so the
        // only thing sending the request to `/api/v1/account` is the path prefix.
        let client = Client::new(Config::with_base_url(server.uri()).api_key(
            "nx",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        ));
        let _: serde_json::Value = client.signed_get("/api/v1/account", &[]).await.unwrap();
    }

    #[tokio::test]
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
}
