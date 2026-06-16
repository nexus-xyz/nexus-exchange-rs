//! Client configuration.

use crate::auth::Credentials;
use std::sync::Arc;

/// Which Nexus Exchange environment to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Network {
    /// Production / stable channel.
    Stable,
    /// Beta channel (tracks `main`; may break).
    Beta,
    /// Local development server.
    Local,
}

impl Network {
    /// Base URL for this network.
    pub fn base_url(self) -> &'static str {
        match self {
            Network::Stable => "https://exchange.nexus.xyz/api/exchange",
            Network::Beta => "https://beta.exchange.nexus.xyz/api/exchange",
            Network::Local => "http://localhost:9090",
        }
    }
}

/// Client configuration. Credentials are optional — public market-data
/// endpoints need none.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: String,
    pub(crate) credentials: Option<Arc<Credentials>>,
}

impl Config {
    /// Target the given [`Network`], unauthenticated.
    pub fn new(network: Network) -> Self {
        Self {
            base_url: network.base_url().to_string(),
            credentials: None,
        }
    }

    /// Target a custom base URL (e.g. a preview deployment), unauthenticated.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            credentials: None,
        }
    }

    /// Authenticate with an HMAC API key — `key_id` and the 64-char hex
    /// `secret` from `POST /keys`.
    pub fn api_key(mut self, key_id: impl Into<String>, secret: impl Into<String>) -> Self {
        self.credentials = Some(Arc::new(Credentials::api_key(key_id, secret)));
        self
    }

    /// Authenticate with a session bearer token from `POST /auth/login`.
    pub fn session_token(mut self, token: impl Into<String>) -> Self {
        self.credentials = Some(Arc::new(Credentials::session(token)));
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new(Network::Stable)
    }
}
