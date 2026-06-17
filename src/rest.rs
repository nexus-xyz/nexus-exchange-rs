//! REST endpoint methods on [`crate::Client`].
//!
//! Public, unauthenticated market-data endpoints. Account/trading and admin
//! endpoints are added in follow-up PRs.

use std::collections::HashMap;

use crate::types::{
    AccountSummary, ApiKeyInfo, Fill, FundingSample, HealthStatus, MarkPrice, Market, MarketStatus,
    MarketSummary, Ohlcv, OrderBook, Position, RateLimitStatus, Ticker, Trade,
};
use crate::{Client, Result};

impl Client {
    /// List all tradable markets and their trading rules.
    pub async fn fetch_markets(&self) -> Result<Vec<Market>> {
        self.get("/markets", &[]).await
    }

    /// Per-market summaries with 24h volume and halt state.
    pub async fn fetch_market_summaries(&self) -> Result<Vec<MarketSummary>> {
        self.get("/markets/summary", &[]).await
    }

    /// Tickers for all markets, keyed by symbol.
    pub async fn fetch_tickers(&self) -> Result<HashMap<String, Ticker>> {
        self.get("/tickers", &[]).await
    }

    /// Fetch the ticker for a single market, e.g. `BTC-USDX-PERP`.
    pub async fn fetch_ticker(&self, market_id: &str) -> Result<Ticker> {
        self.get(&format!("/markets/{market_id}/ticker"), &[]).await
    }

    /// Order book snapshot for a market.
    pub async fn fetch_order_book(&self, market_id: &str) -> Result<OrderBook> {
        self.get(&format!("/markets/{market_id}/orderbook"), &[])
            .await
    }

    /// Recent public trades for a market (newest first), optionally limited.
    pub async fn fetch_trades(&self, market_id: &str, limit: Option<u32>) -> Result<Vec<Trade>> {
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.get(&format!("/markets/{market_id}/trades"), &query)
            .await
    }

    /// OHLCV candles for a market.
    pub async fn fetch_ohlcv(
        &self,
        market_id: &str,
        timeframe: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<Ohlcv>> {
        let mut query = Vec::new();
        if let Some(timeframe) = timeframe {
            query.push(("timeframe", timeframe.to_string()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.get(&format!("/markets/{market_id}/candles"), &query)
            .await
    }

    /// Intra-hour funding-rate history for a market.
    pub async fn fetch_funding_rate_history(
        &self,
        market_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<FundingSample>> {
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.get(&format!("/markets/{market_id}/funding"), &query)
            .await
    }

    /// Current mark price for a market.
    pub async fn fetch_mark_price(&self, market_id: &str) -> Result<MarkPrice> {
        self.get(&format!("/markets/{market_id}/mark-price"), &[])
            .await
    }

    /// Lifecycle / halt status for a market.
    pub async fn fetch_market_status(&self, market_id: &str) -> Result<MarketStatus> {
        self.get(&format!("/markets/{market_id}/status"), &[]).await
    }

    /// Indexer health/status snapshot. Unauthenticated.
    pub async fn health_check(&self) -> Result<HealthStatus> {
        self.get("/health", &[]).await
    }

    /// List the API keys for the authenticated session. Requires credentials.
    pub async fn fetch_api_keys(&self) -> Result<Vec<ApiKeyInfo>> {
        self.signed_get("/keys", &[]).await
    }

    /// Account balance and collateral summary. Requires credentials.
    pub async fn fetch_balance(&self) -> Result<AccountSummary> {
        self.signed_get("/account", &[]).await
    }

    /// Open positions for the authenticated account. Requires credentials.
    pub async fn fetch_positions(&self) -> Result<Vec<Position>> {
        self.signed_get("/positions", &[]).await
    }

    /// Recent fills (private trade executions) for the authenticated account.
    /// Requires credentials.
    pub async fn fetch_my_trades(&self) -> Result<Vec<Fill>> {
        self.signed_get("/fills", &[]).await
    }

    /// Current rate-limit status for the caller. Requires credentials.
    pub async fn fetch_rate_limit_status(&self) -> Result<RateLimitStatus> {
        self.signed_get("/account/rate-limit", &[]).await
    }
}
