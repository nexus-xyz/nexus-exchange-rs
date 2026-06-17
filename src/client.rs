//! The HTTP client — entry point for the SDK.

use crate::{Config, Error, Result};
use serde::de::DeserializeOwned;

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
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    pub(crate) config: Config,
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
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = format!("{}{}", self.config.base_url, path);
        let resp = self.http.get(url).query(query).send().await?;
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
