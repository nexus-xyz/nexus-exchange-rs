//! The HTTP client — entry point for the SDK.

use crate::{Config, Error, Result};
use serde::{de::DeserializeOwned, Serialize};
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
/// streaming in [`crate::ws`].
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

    /// Unauthenticated `GET`.
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
        let mut req = self.http.get(url);
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
            .request(method, format!("{}{}", self.config.base_url, path));
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
