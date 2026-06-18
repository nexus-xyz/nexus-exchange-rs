//! The HTTP client — entry point for the SDK.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::ratelimit::{RateLimiter, ThrottleInfo};
use crate::types::RateLimitStatus;
use crate::{Config, Error, Result};

/// The `{ code, message }` error envelope returned by the API on failures.
#[derive(serde::Deserialize)]
struct ApiErrorBody {
    code: String,
    message: Option<String>,
}

/// Entry point for the Nexus Exchange API.
///
/// Construct with [`Client::new`]. REST methods live in [`crate::rest`];
/// streaming in [`crate::ws`] (added incrementally).
///
/// The client paces itself against the server's rate limit: it honors `429` +
/// `Retry-After` (retrying up to [`RateLimit::max_retries`](crate::RateLimit::max_retries))
/// and, when enabled, proactively meters requests through a cost-weighted token
/// bucket. Call [`Client::fetch_rate_limit_status`] to sync that bucket to the
/// caller's live server-side budget.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    config: Config,
    limiter: Arc<RateLimiter>,
}

impl Client {
    /// Create a client for the given [`Config`].
    pub fn new(config: Config) -> Self {
        let limiter = Arc::new(RateLimiter::new(&config.rate_limit));
        Self {
            http: reqwest::Client::new(),
            config,
            limiter,
        }
    }

    /// The configured base URL.
    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// Issue a `GET` and deserialize the JSON response, decoding the API's
    /// `{ code, message }` envelope on non-2xx.
    ///
    /// `cost` is the endpoint's rate-limit weight: it is reserved from the token
    /// bucket before the request goes out (0 for endpoints the server does not
    /// charge). On `429` the request is retried, honoring `Retry-After`, up to
    /// the configured retry ceiling.
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
        loop {
            let resp = self.http.get(&url).query(query).send().await?;
            let status = resp.status();

            if status.as_u16() == 429 {
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

            let bytes = resp.bytes().await?;
            return if status.is_success() {
                Ok(serde_json::from_slice(&bytes)?)
            } else if let Ok(env) = serde_json::from_slice::<ApiErrorBody>(&bytes) {
                Err(Error::Api {
                    code: env.code,
                    message: env.message.unwrap_or_default(),
                })
            } else {
                Err(Error::Api {
                    code: status.as_str().to_string(),
                    message: String::from_utf8_lossy(&bytes).into_owned(),
                })
            };
        }
    }

    /// Sync the client-side limiter to a server-reported rate-limit snapshot.
    pub(crate) fn sync_rate_limit(&self, status: &RateLimitStatus) {
        self.limiter.sync(status.limit, status.remaining);
    }
}
