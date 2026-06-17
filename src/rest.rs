//! REST endpoint methods on [`crate::Client`].
//!
//! Added incrementally by route group: public market data, account & trading,
//! admin. Skeleton.
//!
//! List endpoints return an auto-paging [`pagination::Paginator`] rather than a
//! bare page, so callers never have to drive cursors by hand.

pub mod pagination;

pub use pagination::{Cursor, Page, PageRequest, Paginator};

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
