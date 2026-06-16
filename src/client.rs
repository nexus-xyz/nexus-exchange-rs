//! The HTTP client — entry point for the SDK.

use crate::Config;

/// Entry point for the Nexus Exchange API.
///
/// Construct with [`Client::new`]. REST methods live in [`crate::rest`] and
/// streaming in [`crate::ws`] (added incrementally).
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) http: reqwest::Client,
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

    #[allow(dead_code)]
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }
}
