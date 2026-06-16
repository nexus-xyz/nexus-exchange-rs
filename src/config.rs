//! Client configuration.

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

/// Client configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: String,
}

impl Config {
    /// Target the given [`Network`].
    pub fn new(network: Network) -> Self {
        Self {
            base_url: network.base_url().to_string(),
        }
    }

    /// Target a custom base URL (e.g. a preview deployment).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new(Network::Stable)
    }
}
