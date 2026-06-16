//! The HTTP client — entry point for the SDK.

use crate::{Config, Error, Result};
use serde::de::DeserializeOwned;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Entry point for the Nexus Exchange API.
///
/// Construct with [`Client::new`]. REST methods live in [`crate::rest`];
/// streaming in [`crate::ws`] (added incrementally).
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

    /// Issue an unauthenticated `GET` and deserialize the JSON response.
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let resp = self
            .http
            .get(format!("{}{}", self.config.base_url, path))
            .query(query)
            .send()
            .await?;
        self.handle(resp).await
    }

    /// Issue an HMAC/bearer-signed `GET`. Signs the exact path + query string,
    /// then deserializes the JSON response.
    pub(crate) async fn signed_get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let creds = self
            .config
            .credentials
            .as_ref()
            .ok_or_else(|| Error::Auth("this endpoint requires credentials".into()))?;

        // Build the query string once so the signed bytes match what is sent.
        let qs = serde_urlencoded::to_string(query).unwrap_or_default();
        let headers = creds.headers("GET", path, &qs, b"", now_ms())?;

        let url = if qs.is_empty() {
            format!("{}{}", self.config.base_url, path)
        } else {
            format!("{}{}?{}", self.config.base_url, path, qs)
        };
        let mut req = self.http.get(url);
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
                code: env.code,
                message: env.message.unwrap_or_default(),
            })
        } else {
            Err(Error::Api {
                code: status.as_str().to_string(),
                message: String::from_utf8_lossy(&bytes).into_owned(),
            })
        }
    }
}
