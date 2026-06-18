//! REST endpoint methods on [`crate::Client`].
//!
//! Added incrementally by route group: public market data, account & trading,
//! admin. Skeleton.
//!
//! List endpoints return an auto-paging [`pagination::Paginator`] rather than a
//! bare page, so callers never have to drive cursors by hand.

pub mod pagination;

pub use pagination::{Cursor, Page, PageRequest, Paginator};

use std::collections::HashMap;

use crate::types::{
    AccountSummary, ApiKeyInfo, CreditResult, Decimal, DepositResult, Fill, FundingSample,
    HealthStatus, MarkPrice, Market, MarketStatus, MarketSummary, Ohlcv, Order, OrderBook,
    OrderRequest, OrderResponse, Position, RateLimitStatus, Ticker, TierOverride, Trade,
    Withdrawal, WsToken,
};
use crate::{Client, Result};

/// Per-endpoint rate-limit cost weight (CCXT-style) for the proactively metered
/// public `GET`s. The server prices most endpoints at one token. (The signed
/// endpoints go through the auth path, which isn't proactively metered; the
/// free `/account/rate-limit` poll is one of them.)
const COST_DEFAULT: f64 = 1.0;

impl Client {
    /// List all tradable markets and their trading rules.
    pub async fn fetch_markets(&self) -> Result<Vec<Market>> {
        self.get("/markets", &[], COST_DEFAULT).await
    }

    /// Per-market summaries with 24h volume and halt state.
    pub async fn fetch_market_summaries(&self) -> Result<Vec<MarketSummary>> {
        self.get("/markets/summary", &[], COST_DEFAULT).await
    }

    /// Tickers for all markets, keyed by symbol.
    pub async fn fetch_tickers(&self) -> Result<HashMap<String, Ticker>> {
        self.get("/tickers", &[], COST_DEFAULT).await
    }

    /// Fetch the ticker for a single market, e.g. `BTC-USDX-PERP`.
    pub async fn fetch_ticker(&self, market_id: &str) -> Result<Ticker> {
        self.get(&format!("/markets/{market_id}/ticker"), &[], COST_DEFAULT)
            .await
    }

    /// Order book snapshot for a market.
    pub async fn fetch_order_book(&self, market_id: &str) -> Result<OrderBook> {
        self.get(
            &format!("/markets/{market_id}/orderbook"),
            &[],
            COST_DEFAULT,
        )
        .await
    }

    /// Recent public trades for a market (newest first), optionally limited.
    pub async fn fetch_trades(&self, market_id: &str, limit: Option<u32>) -> Result<Vec<Trade>> {
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.get(
            &format!("/markets/{market_id}/trades"),
            &query,
            COST_DEFAULT,
        )
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
        self.get(
            &format!("/markets/{market_id}/candles"),
            &query,
            COST_DEFAULT,
        )
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
        self.get(
            &format!("/markets/{market_id}/funding"),
            &query,
            COST_DEFAULT,
        )
        .await
    }

    /// Current mark price for a market.
    pub async fn fetch_mark_price(&self, market_id: &str) -> Result<MarkPrice> {
        self.get(
            &format!("/markets/{market_id}/mark-price"),
            &[],
            COST_DEFAULT,
        )
        .await
    }

    /// Lifecycle / halt status for a market.
    pub async fn fetch_market_status(&self, market_id: &str) -> Result<MarketStatus> {
        self.get(&format!("/markets/{market_id}/status"), &[], COST_DEFAULT)
            .await
    }

    /// Indexer health/status snapshot. Unauthenticated.
    pub async fn health_check(&self) -> Result<HealthStatus> {
        self.get("/health", &[], COST_DEFAULT).await
    }

    /// Fetch the caller's current rate-limit status (tier, ceiling, remaining,
    /// reset) and sync the client-side limiter to it. Requires credentials.
    ///
    /// This endpoint does not consume a rate-limit token, so it can be polled
    /// freely to self-pace. Calling it teaches the client the caller's real
    /// tier, so subsequent requests are metered against the actual server-side
    /// budget instead of the conservative default.
    pub async fn fetch_rate_limit_status(&self) -> Result<RateLimitStatus> {
        let status: RateLimitStatus = self.signed_get("/account/rate-limit", &[]).await?;
        self.sync_rate_limit(&status);
        Ok(status)
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

    /// Place a single order. Requires credentials.
    pub async fn create_order(&self, order: &OrderRequest) -> Result<OrderResponse> {
        self.signed_post("/orders", order).await
    }

    /// Submit a batch of orders (sequential, non-atomic). Requires credentials.
    /// The per-order result array is currently untyped in the spec.
    pub async fn create_orders(&self, orders: &[OrderRequest]) -> Result<serde_json::Value> {
        self.signed_post("/orders/batch", &orders).await
    }

    /// Cancel a single order by id. Requires credentials.
    pub async fn cancel_order(&self, order_id: &str) -> Result<serde_json::Value> {
        self.signed_delete(&format!("/orders/{order_id}")).await
    }

    /// Cancel all open orders for the account. Requires credentials.
    pub async fn cancel_all_orders(&self) -> Result<serde_json::Value> {
        self.signed_delete("/orders").await
    }

    /// List open orders for the authenticated account. Requires credentials.
    pub async fn fetch_open_orders(&self) -> Result<Vec<Order>> {
        self.signed_get("/orders", &[]).await
    }

    /// Fetch a single order by id. Requires credentials.
    pub async fn fetch_order(&self, order_id: &str) -> Result<Order> {
        self.signed_get(&format!("/orders/{order_id}"), &[]).await
    }

    /// Deposit USDX collateral. Requires credentials.
    pub async fn deposit(&self, amount: Decimal) -> Result<DepositResult> {
        self.signed_post(
            "/account/deposit",
            &serde_json::json!({ "amount": amount.to_string() }),
        )
        .await
    }

    /// Withdrawal history for the authenticated account. Requires credentials.
    pub async fn fetch_withdrawals(&self) -> Result<Vec<Withdrawal>> {
        self.signed_get("/withdrawals", &[]).await
    }

    /// Claim synthetic (testnet) USDX from the faucet, up to the per-key daily
    /// allowance. Omit `amount` to claim the full remaining allowance. Requires
    /// credentials.
    pub async fn claim_credit(&self, amount: Option<Decimal>) -> Result<CreditResult> {
        let body = match amount {
            Some(a) => serde_json::json!({ "amount": a.to_string() }),
            None => serde_json::json!({}),
        };
        self.signed_post("/account/credit", &body).await
    }

    /// Set an account's rate-limit tier (admin). Requires admin credentials.
    pub async fn set_account_tier(&self, address: &str, tier: &str) -> Result<TierOverride> {
        self.signed_put(
            "/admin/tiers",
            &serde_json::json!({ "address": address, "tier": tier }),
        )
        .await
    }

    /// List tier overrides (admin). Requires admin credentials.
    pub async fn fetch_tier_overrides(&self) -> Result<Vec<TierOverride>> {
        self.signed_get("/admin/tiers", &[]).await
    }

    /// Reset an account to its default tier (admin). Requires admin credentials.
    pub async fn reset_account_tier(&self, address: &str) -> Result<serde_json::Value> {
        self.signed_delete(&format!("/admin/tiers/{address}")).await
    }

    /// Mint a single-use, short-lived WebSocket token for the WebSocket
    /// streaming client. Requires credentials.
    pub async fn mint_web_socket_token(&self) -> Result<WsToken> {
        self.signed_post_empty("/ws/token").await
    }
}
