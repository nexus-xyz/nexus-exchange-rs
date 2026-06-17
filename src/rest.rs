//! REST endpoint methods on [`crate::Client`].
//!
//! Public, unauthenticated market-data endpoints. Account/trading and admin
//! endpoints are added in follow-up PRs.

use crate::types::{HealthStatus, Market, Ticker};
use crate::{Client, Result};

impl Client {
    /// List all tradable markets and their trading rules.
    pub async fn fetch_markets(&self) -> Result<Vec<Market>> {
        self.get("/markets", &[]).await
    }

    /// Fetch the ticker for a single market, e.g. `BTC-USDX-PERP`.
    pub async fn fetch_ticker(&self, market_id: &str) -> Result<Ticker> {
        self.get(&format!("/markets/{market_id}/ticker"), &[]).await
    }

    /// Indexer health/status snapshot. Unauthenticated.
    pub async fn health_check(&self) -> Result<HealthStatus> {
        self.get("/health", &[]).await
    }
}
