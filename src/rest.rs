//! REST endpoint methods on [`crate::Client`].
//!
//! Public, unauthenticated market-data endpoints. Account/trading and admin
//! endpoints are added in follow-up PRs.

use crate::types::{HealthStatus, Market, RateLimitStatus, Ticker};
use crate::{Client, Result};

/// Per-endpoint rate-limit cost weights (CCXT-style). The server prices most
/// endpoints at one token; `/account/rate-limit` is free to poll.
const COST_DEFAULT: f64 = 1.0;
const COST_FREE: f64 = 0.0;

impl Client {
    /// List all tradable markets and their trading rules.
    pub async fn fetch_markets(&self) -> Result<Vec<Market>> {
        self.get("/markets", &[], COST_DEFAULT).await
    }

    /// Fetch the ticker for a single market, e.g. `BTC-USDX-PERP`.
    pub async fn fetch_ticker(&self, market_id: &str) -> Result<Ticker> {
        self.get(&format!("/markets/{market_id}/ticker"), &[], COST_DEFAULT)
            .await
    }

    /// Indexer health/status snapshot. Unauthenticated.
    pub async fn health_check(&self) -> Result<HealthStatus> {
        self.get("/health", &[], COST_DEFAULT).await
    }

    /// Fetch the caller's current rate-limit status (tier, ceiling, remaining,
    /// reset) and sync the client-side limiter to it.
    ///
    /// This endpoint does not consume a rate-limit token, so it can be polled
    /// freely to self-pace. Calling it teaches the client the caller's real
    /// tier, so subsequent requests are metered against the actual server-side
    /// budget instead of the conservative default.
    pub async fn fetch_rate_limit_status(&self) -> Result<RateLimitStatus> {
        let status: RateLimitStatus = self.get("/account/rate-limit", &[], COST_FREE).await?;
        self.sync_rate_limit(&status);
        Ok(status)
    }
}
