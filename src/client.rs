//! The HTTP client — entry point for the SDK.

use backon::Retryable;
use serde::de::DeserializeOwned;

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
/// Each request is bounded by [`Config::with_timeout`] and, on
/// [transient](Error::is_transient) failures, retried with exponential backoff
/// per [`Config::with_retry`].
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    config: Config,
}

impl Client {
    /// Create a client for the given [`Config`].
    pub fn new(config: Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
        }
    }

    /// The configured base URL.
    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// Issue a `GET` and deserialize the JSON response, decoding the API's
    /// `{ code, message }` envelope on non-2xx.
    ///
    /// The send is retried on transient failures; the final JSON
    /// deserialization is not (a malformed body fails identically on retry).
    ///
    /// **Retry safety:** this wraps the request in the retry layer, so it must
    /// only be used for idempotent methods (`GET`). A transient failure on a
    /// non-idempotent request (e.g. a `POST` that places an order) can mean the
    /// server applied it but the response was lost — a blind retry would
    /// double-submit. Future non-idempotent endpoints must use a separate,
    /// non-retrying path (or a client-supplied idempotency key), not this
    /// helper. Tracked in
    /// <https://github.com/nexus-xyz/nexus-exchange-rs/issues/27>.
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = format!("{}{}", self.config.base_url, path);
        let bytes = (|| self.send_get(&url, query))
            .retry(self.config.retry.backoff())
            .when(Error::is_transient)
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Perform a single `GET` attempt: send the request, read the body, and map
    /// a non-2xx status onto [`Error::Api`]. One success or error here is one
    /// unit the retry layer may repeat.
    async fn send_get(&self, url: &str, query: &[(&str, String)]) -> Result<bytes::Bytes> {
        let resp = self
            .http
            .get(url)
            .query(query)
            .timeout(self.config.timeout)
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if status.is_success() {
            Ok(bytes)
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
}
