//! The HTTP client — entry point for the SDK.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use backon::BackoffBuilder;
use serde::{de::DeserializeOwned, Serialize};

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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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
        let url = format!("{}{}", self.config.base_url, path);

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
                    // Only connect/timeout transport errors are transient; a
                    // mid-body-read failure is not, and exhausted backoff
                    // surfaces the error.
                    let err = Error::Http(e);
                    if err.is_transient() {
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
                return Err(Error::RateLimited {
                    retry_after: info.retry_after,
                });
            }

            // A 5xx / 408 response is transient: retry per the backoff before
            // giving up. Success and terminal errors return unchanged.
            match self.handle(resp).await {
                Err(err) if err.is_transient() => {
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

    /// Signed `GET` — signs the exact path + query string that is sent.
    pub(crate) async fn signed_get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let creds = self.creds()?;
        let qs = serde_urlencoded::to_string(query).unwrap_or_default();
        let headers = creds.headers("GET", path, &qs, b"", now_ms())?;
        let url = if qs.is_empty() {
            format!("{}{}", self.config.base_url, path)
        } else {
            format!("{}{}?{}", self.config.base_url, path, qs)
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

    /// Signed `DELETE` (no body).
    pub(crate) async fn signed_delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.signed_no_body(reqwest::Method::DELETE, path).await
    }

    /// Signed `POST` with no body (e.g. token mint).
    pub(crate) async fn signed_post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.signed_no_body(reqwest::Method::POST, path).await
    }

    fn creds(&self) -> Result<&crate::auth::Credentials> {
        self.config
            .credentials
            .as_deref()
            .ok_or_else(|| Error::Auth("this endpoint requires credentials".into()))
    }

    async fn signed_with_body<B: Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let body_bytes = serde_json::to_vec(body)?;
        let headers = self
            .creds()?
            .headers(method.as_str(), path, "", &body_bytes, now_ms())?;
        let mut req = self
            .http
            .request(method, format!("{}{}", self.config.base_url, path))
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
    ) -> Result<T> {
        let headers = self
            .creds()?
            .headers(method.as_str(), path, "", b"", now_ms())?;
        let mut req = self
            .http
            .request(method, format!("{}{}", self.config.base_url, path))
            .timeout(self.config.timeout);
        for (name, value) in &headers {
            req = req.header(*name, value);
        }
        self.handle(req.send().await?).await
    }

    /// Decode a response, mapping the `{ code, message }` envelope on non-2xx.
    async fn handle<T: DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if status.is_success() {
            Ok(serde_json::from_slice(&bytes)?)
        } else if let Ok(env) = serde_json::from_slice::<ApiErrorBody>(&bytes) {
            Err(Error::Api {
                status: status.as_u16(),
                code: env.code,
                message: env.message.unwrap_or_default(),
            })
        } else {
            Err(Error::Api {
                status: status.as_u16(),
                code: status.as_str().to_string(),
                message: String::from_utf8_lossy(&bytes).into_owned(),
            })
        }
    }

    /// Sync the client-side limiter to a server-reported rate-limit snapshot.
    pub(crate) fn sync_rate_limit(&self, status: &RateLimitStatus) {
        self.limiter.sync(status.limit, status.remaining);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use wiremock::matchers::{header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
