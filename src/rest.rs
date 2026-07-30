//! REST endpoint methods on [`crate::Client`].
//!
//! Added incrementally by route group: public market data, account & trading,
//! admin. Skeleton.
//!
//! **Dual-stack routing (ENG-4947 / gateway elimination ENG-4740).** Endpoints
//! whose path begins with `/api/v1/` are served directly by the indexer at the
//! host root ([`Config::direct_base_url`](crate::Config::direct_base_url));
//! every other path stays on the legacy `/api/exchange` gateway base. The
//! [`Client`] picks the base off the path prefix, so the method here just names
//! the full path it targets. Market-data and account/trading endpoints have been
//! migrated to `/api/v1`; endpoints without a `/api/v1` variant yet (health,
//! keys, agents, wallet auth, deposits/withdrawals, ADL, admin, WebSocket-token,
//! `GET /orders/{id}`, and the tier-3 endpoints) remain on the gateway until the
//! spec grows those variants.
//!
//! List endpoints return an auto-paging [`pagination::Paginator`] rather than a
//! bare page, so callers never have to drive cursors by hand.

pub mod pagination;

pub use pagination::{Cursor, Page, PageRequest, Paginator};

use std::collections::HashMap;

use crate::auth::{AgentRegistration, EthSigner};
use crate::types::{
    AccountFees, AccountPortfolioSummary, AccountState, AccountSummary, AdlEvent, AgentInfo,
    AgentRegistered, AmendOrder, ApiKeyInfo, BridgeAssetsResponse, BridgeDeposit,
    BridgeDepositAddress, CancelOnDisconnectStatus, ClosedPosition, CreatedApiKey, CreditResult,
    Decimal, DepositResult, EquityPoint, Fill, FundingPayment, FundingSample, HealthStatus,
    LeverageUpdate, LoginResponse, MarginAdjustment, MarginDirection, MarginMode, MarginModeUpdate,
    MarkPrice, Market, MarketStatus, MarketSummary, Ohlcv, Order, OrderBook, OrderHistoryEntry,
    OrderPreview, OrderRequest, OrderResponse, OrderResult, PortfolioHistory, PortfolioWindow,
    Position, RateLimitStatus, SubAccount, Ticker, TierOverride, Trade, Transfer, TransferRequest,
    Withdrawal, WsToken,
};
use crate::{Client, Error, Result};

/// The exact message a wallet must EIP-191 `personal_sign` to authenticate via
/// [`Client::login`]. Sign these bytes with your wallet; the resulting
/// signature is what `login` exchanges for a session token.
pub const LOGIN_MESSAGE: &str = "Sign in to Nexus Exchange";

/// Per-endpoint rate-limit cost weight (CCXT-style) for the proactively metered
/// public `GET`s. The server prices most endpoints at one token. (The signed
/// endpoints go through the auth path, which isn't proactively metered; the
/// free `/account/rate-limit` poll is one of them.)
const COST_DEFAULT: f64 = 1.0;

/// Largest `limit` the portfolio-history request schema permits (`maximum: 366`,
/// the capacity of the widest window). Enforced locally by
/// [`Client::fetch_portfolio_history`] so a request that violates the schema is
/// never signed or sent; kept public so callers can clamp their own input to the
/// same bound instead of hard-coding it.
pub const MAX_PORTFOLIO_HISTORY_LIMIT: u32 = 366;

/// Largest `limit` (page size) the `GET /api/v1/markets/{id}/trades` request
/// schema permits (`maximum: 1000`, default 100).
///
/// Enforced by [`Client::fetch_trades_paginated`] before a page is fetched, so an
/// out-of-schema [`Paginator::page_size`] fails locally instead of costing a
/// round trip.
pub const MAX_TRADES_LIMIT: u32 = 1000;

/// Largest `limit` (page size) the `GET /api/v1/fills` request schema permits
/// (`maximum: 1000`, default 100). Enforced by
/// [`Client::fetch_my_trades_paginated`].
///
/// The paginated `limit` maxima are **per endpoint** in the spec (trades and
/// fills 1000, `/orders/history` 500, `/positions/closed` 200,
/// `/account/equity-history` 720) and are not interchangeable. In particular none
/// of them is [`MAX_PORTFOLIO_HISTORY_LIMIT`]: that bound belongs to
/// `/account/portfolio-history`, which is not cursor-paginated at all.
pub const MAX_FILLS_LIMIT: u32 = 1000;

/// Largest `limit` (page size) the `GET /api/v1/orders/history` request schema
/// permits (`maximum: 500`, default 100) — half what `/fills` and the public
/// trades feed allow. Enforced by [`Client::fetch_order_history`] and
/// [`Client::fetch_order_history_paginated`].
pub const MAX_ORDER_HISTORY_LIMIT: u32 = 500;

/// Largest `limit` (page size) the `GET /api/v1/positions/closed` request schema
/// permits (`maximum: 200`, default 100) — the **smallest** of the five paginated
/// maxima, so a long close history takes proportionally more pages than `/fills`
/// would. Enforced by [`Client::fetch_closed_positions`] and
/// [`Client::fetch_closed_positions_paginated`].
pub const MAX_CLOSED_POSITIONS_LIMIT: u32 = 200;

/// Largest `limit` (page size) the `GET /api/v1/account/equity-history` request
/// schema permits (`maximum: 720`) — which is also that endpoint's **default**.
/// Enforced by [`Client::fetch_equity_history`] and
/// [`Client::fetch_equity_history_paginated`].
///
/// Uniquely among the paginated endpoints, omitting `limit` here asks for 720
/// points rather than 100: one page already covers the whole ~1h / 5s window. Any
/// smaller shared clamp — [`MAX_PORTFOLIO_HISTORY_LIMIT`]'s `366` in particular —
/// would sit *below* that default and reject client-side a plain request the
/// server accepts.
pub const MAX_EQUITY_HISTORY_LIMIT: u32 = 720;

/// Reject a page size the endpoint's request schema forbids, before it is sent
/// (and, on a signed route, before it is signed).
///
/// Checked inside the page-fetching closure rather than at paginator
/// construction because [`Paginator::page_size`] is a builder method that cannot
/// return an error — this way an out-of-range size fails on the first page fetch
/// instead of being silently sent and rejected by the server. The flat
/// first-page methods run the same check before signing.
fn check_page_size(limit: Option<u32>, maximum: u32, endpoint: &str) -> Result<()> {
    match limit {
        Some(limit) if limit == 0 || limit > maximum => Err(Error::invalid_request(format!(
            "{endpoint} page size must be between 1 and {maximum} (got {limit})"
        ))),
        _ => Ok(()),
    }
}

/// Build the query for a flat, first-page-only read of a paginated list endpoint:
/// the `limit` when one was given, and nothing else.
///
/// No `cursor` — that is what makes these methods first-page-only, and it is why
/// they can keep returning a plain `Vec` while the `*_paginated` variants own the
/// walk.
fn limit_query(limit: Option<u32>) -> Vec<(&'static str, String)> {
    match limit {
        Some(limit) => vec![("limit", limit.to_string())],
        None => Vec::new(),
    }
}

/// Build the `limit` / `cursor` query for one page of a paginated list endpoint.
///
/// The cursor is passed back **verbatim** — it is opaque, and the encoder is the
/// only thing that touches it, so what is signed always equals what is sent.
fn page_query(req: &PageRequest) -> Vec<(&'static str, String)> {
    let mut query = Vec::new();
    if let Some(limit) = req.limit {
        query.push(("limit", limit.to_string()));
    }
    if let Some(cursor) = &req.cursor {
        query.push(("cursor", cursor.as_str().to_string()));
    }
    query
}

/// Percent-encode a single path segment so a caller-supplied identifier (e.g. a
/// client order id) cannot break out of its position in the request path.
/// Everything outside the RFC 3986 *unreserved* set is escaped, so `/`, `?`,
/// `#`, `..`, whitespace, etc. become `%XX` rather than altering the path that
/// is both signed and sent — keeping `signed === sent` and ruling out path
/// traversal / injection through untrusted identifiers.
fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Reject an empty identifier and percent-encode the rest for safe use as a
/// path segment. Keeps a blank id from collapsing `/orders/by-client-id/{id}`
/// into the parent collection route.
fn encoded_segment(value: &str, name: &str) -> Result<String> {
    if value.is_empty() {
        return Err(Error::invalid_request(format!("{name} must not be empty")));
    }
    Ok(encode_path_segment(value))
}

/// Reject a blank identifier carried in a request *body* or query (not the
/// path). Mirrors the [`encoded_segment`] guard so body-borne ids are validated
/// as consistently as path-borne ones, just without the percent-encoding.
///
/// Rejects whitespace-only as well as empty: a blank identifier is never a
/// legitimate market/order id, and for a scoped cancel a `" "` market would
/// otherwise be sent (server-rejected as unknown) — tightening it here keeps
/// the rejection local and the "no silent account-wide flatten" guard airtight.
fn require_non_empty(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::invalid_request(format!("{name} must not be blank")));
    }
    Ok(())
}

impl Client {
    /// List all tradable markets and their trading rules.
    pub async fn fetch_markets(&self) -> Result<Vec<Market>> {
        self.get("/markets", &[], COST_DEFAULT).await
    }

    /// Per-market summaries with 24h volume and halt state.
    pub async fn fetch_market_summaries(&self) -> Result<Vec<MarketSummary>> {
        self.get("/api/v1/markets/summary", &[], COST_DEFAULT).await
    }

    /// Tickers for all markets, keyed by market id (e.g. `BTC-USDX-PERP`).
    ///
    /// The envelope is a bare JSON object whose keys are market ids and whose
    /// values are [`Ticker`]s (spec: `additionalProperties: Ticker`, *"Object
    /// keyed by market_id"*) — there is no wrapper. The spec ships no `example`
    /// for this route, but the response *schema* fixes the shape, so the map
    /// model is authoritative; an empty result is `{}`, which decodes to an
    /// empty map.
    pub async fn fetch_tickers(&self) -> Result<HashMap<String, Ticker>> {
        self.get("/api/v1/tickers", &[], COST_DEFAULT).await
    }

    /// Fetch the ticker for a single market, e.g. `BTC-USDX-PERP`.
    pub async fn fetch_ticker(&self, market_id: &str) -> Result<Ticker> {
        let id = encoded_segment(market_id, "market_id")?;
        self.get(&format!("/api/v1/markets/{id}/ticker"), &[], COST_DEFAULT)
            .await
    }

    /// Order book snapshot for a market.
    pub async fn fetch_order_book(&self, market_id: &str) -> Result<OrderBook> {
        let id = encoded_segment(market_id, "market_id")?;
        self.get(
            &format!("/api/v1/markets/{id}/orderbook"),
            &[],
            COST_DEFAULT,
        )
        .await
    }

    /// Recent public trades for a market (newest first), optionally limited.
    pub async fn fetch_trades(&self, market_id: &str, limit: Option<u32>) -> Result<Vec<Trade>> {
        let id = encoded_segment(market_id, "market_id")?;
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.get(
            &format!("/api/v1/markets/{id}/trades"),
            &query,
            COST_DEFAULT,
        )
        .await
    }

    /// Every recent public trade for a market, as an auto-paging
    /// [`Paginator`] that follows the `X-Next-Cursor` header for you.
    ///
    /// [`fetch_trades`](Self::fetch_trades) returns the first page only; this
    /// walks the whole history. Nothing is requested until a page is asked for,
    /// so building a paginator is free:
    ///
    /// ```no_run
    /// # use nexus_exchange::{Client, Config, Result};
    /// # async fn run(client: &Client) -> Result<()> {
    /// // Everything, in one Vec.
    /// let trades = client.fetch_trades_paginated("BTC-USDX-PERP")?.page_size(500).all().await?;
    ///
    /// // Or page-by-page, keeping the cursor to resume later.
    /// let mut pager = client.fetch_trades_paginated("BTC-USDX-PERP")?.max_pages(10);
    /// while let Some(page) = pager.next_page().await? {
    ///     let resume_from = page.next_cursor.clone(); // `None` on the last page
    ///     let _ = (page.items, resume_from);
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// [`page_size`](Paginator::page_size) sets the per-page `limit` and must be
    /// in `1..=`[`MAX_TRADES_LIMIT`]; an out-of-range value fails on the first
    /// page fetch rather than being sent. `Err` here means the `market_id` itself
    /// was rejected, so it is reported before any request is issued.
    pub fn fetch_trades_paginated(&self, market_id: &str) -> Result<Paginator<Trade>> {
        // `id` (not a pre-built `path`) is moved into the closure so the path can
        // be `format!`ed *inline at the call site*: scripts/check_spec_drift.py
        // reads the path literal passed to each helper call, and a path built into
        // a local first is invisible to it. See its inline-literal convention.
        let id = encoded_segment(market_id, "market_id")?;
        let client = self.clone();
        Ok(Paginator::new(move |req: PageRequest| {
            let client = client.clone();
            let id = id.clone();
            async move {
                check_page_size(req.limit, MAX_TRADES_LIMIT, "trades")?;
                let (items, next) = client
                    .get_page::<Vec<Trade>>(
                        &format!("/api/v1/markets/{id}/trades"),
                        &page_query(&req),
                        COST_DEFAULT,
                    )
                    .await?;
                Ok(Page::new(items, next))
            }
        }))
    }

    /// OHLCV candles for a market.
    pub async fn fetch_ohlcv(
        &self,
        market_id: &str,
        timeframe: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<Ohlcv>> {
        let id = encoded_segment(market_id, "market_id")?;
        let mut query = Vec::new();
        if let Some(timeframe) = timeframe {
            query.push(("timeframe", timeframe.to_string()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.get(
            &format!("/api/v1/markets/{id}/candles"),
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
        let id = encoded_segment(market_id, "market_id")?;
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.get(
            &format!("/api/v1/markets/{id}/funding"),
            &query,
            COST_DEFAULT,
        )
        .await
    }

    /// Current mark price for a market.
    pub async fn fetch_mark_price(&self, market_id: &str) -> Result<MarkPrice> {
        let id = encoded_segment(market_id, "market_id")?;
        self.get(
            &format!("/api/v1/markets/{id}/mark-price"),
            &[],
            COST_DEFAULT,
        )
        .await
    }

    /// Lifecycle / halt status for a market.
    pub async fn fetch_market_status(&self, market_id: &str) -> Result<MarketStatus> {
        let id = encoded_segment(market_id, "market_id")?;
        self.get(&format!("/api/v1/markets/{id}/status"), &[], COST_DEFAULT)
            .await
    }

    /// ADL settlement events for a market, most recent first (v0.21). `limit`
    /// caps the number of events (server default 100, max 1000).
    ///
    /// Requires API-key credentials (see
    /// [`Config::api_key`](crate::Config::api_key)): the endpoint is HMAC-gated
    /// server-side (`hmacAuth`), not a public market-data read, so the call is
    /// signed and rejected without credentials.
    pub async fn fetch_market_adl_events(
        &self,
        market_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AdlEvent>> {
        let id = encoded_segment(market_id, "market_id")?;
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.signed_get(&format!("/markets/{id}/adl-events"), &query)
            .await
    }

    /// ADL settlement events touching an account, where `address` was the
    /// bankrupt target or a closed counterparty (v0.21). `limit` caps the
    /// number of events (server default 100, max 1000).
    ///
    /// Requires API-key credentials (see
    /// [`Config::api_key`](crate::Config::api_key)): the endpoint is HMAC-gated
    /// server-side (`hmacAuth`), so the call is signed and rejected without
    /// credentials.
    pub async fn fetch_account_adl_history(
        &self,
        address: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AdlEvent>> {
        let addr = encoded_segment(address, "address")?;
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.signed_get(&format!("/account/{addr}/adl-history"), &query)
            .await
    }

    /// Aggregate service health (`GET /status`). Unauthenticated.
    ///
    /// The v0.7.1 spec removed the old liveness `GET /health` / `GET /ready`
    /// probes; `GET /status` is the public health snapshot for the
    /// indexer/engine/oracle/bots. Rely on
    /// [`HealthStatus::status`](crate::types::HealthStatus::status).
    pub async fn health_check(&self) -> Result<HealthStatus> {
        self.get("/status", &[], COST_DEFAULT).await
    }

    /// Fetch the caller's current rate-limit status (tier, ceiling, remaining,
    /// reset) and sync the client-side limiter to it. Requires credentials.
    ///
    /// This endpoint does not consume a rate-limit token, so it can be polled
    /// freely to self-pace. Calling it teaches the client the caller's real
    /// tier, so subsequent requests are metered against the actual server-side
    /// budget instead of the conservative default.
    pub async fn fetch_rate_limit_status(&self) -> Result<RateLimitStatus> {
        let status: RateLimitStatus = self.signed_get("/api/v1/account/rate-limit", &[]).await?;
        self.sync_rate_limit(&status);
        Ok(status)
    }

    /// Exchange an EIP-191 wallet signature for a session bearer token
    /// (`POST /auth/login`). Unauthenticated.
    ///
    /// `signature` is the 0x-prefixed `personal_sign` of [`LOGIN_MESSAGE`] (65
    /// bytes) produced by the caller's wallet — this SDK holds no keys and does
    /// not sign. The message sent is fixed to [`LOGIN_MESSAGE`] so the signed
    /// and submitted bytes can't drift apart. On success, hand
    /// [`LoginResponse::token`] to
    /// [`Config::session_token`](crate::Config::session_token) to authenticate
    /// the `/keys` endpoints.
    pub async fn login(&self, signature: &str) -> Result<LoginResponse> {
        require_non_empty(signature, "signature")?;
        self.post_unsigned(
            "/auth/login",
            &serde_json::json!({ "message": LOGIN_MESSAGE, "signature": signature }),
        )
        .await
    }

    /// List the API keys for the authenticated session. Requires credentials.
    pub async fn fetch_api_keys(&self) -> Result<Vec<ApiKeyInfo>> {
        self.signed_get("/keys", &[]).await
    }

    /// Create a new HMAC API key for the authenticated wallet (`POST /keys`).
    ///
    /// The secret is returned **once** in [`CreatedApiKey::secret`] and is never
    /// shown again — persist it immediately. Requires a session token (see
    /// [`Client::login`] and
    /// [`Config::session_token`](crate::Config::session_token)), the credential
    /// the `/keys` endpoints expect. The SDK signs with whatever credential is
    /// configured and does not enforce the scheme per endpoint, so the server
    /// rejects other credential schemes.
    pub async fn create_api_key(&self) -> Result<CreatedApiKey> {
        self.signed_post_empty("/keys").await
    }

    /// Delete an API key you own, by `key_id` (`DELETE /keys/{key_id}`).
    /// Deleting a key you don't own fails with not-found rather than touching
    /// another wallet. Requires a session token (see
    /// [`Config::session_token`](crate::Config::session_token)), the credential
    /// the `/keys` endpoints expect. As with [`Client::create_api_key`], the SDK
    /// signs with whatever credential is configured and does not enforce the
    /// scheme per endpoint.
    pub async fn delete_api_key(&self, key_id: &str) -> Result<serde_json::Value> {
        let id = encoded_segment(key_id, "key_id")?;
        self.signed_delete(&format!("/keys/{id}")).await
    }

    /// List the non-expired agent keys registered to the authenticated wallet
    /// (`GET /agents`). Requires API-key credentials (see
    /// [`Config::api_key`](crate::Config::api_key)). The SDK signs with whatever
    /// credential is configured and does not enforce the scheme per endpoint.
    pub async fn fetch_agents(&self) -> Result<Vec<AgentInfo>> {
        self.signed_get("/agents", &[]).await
    }

    /// Revoke an agent key by `address` (`DELETE /agents/{address}`). After this
    /// returns, in-flight requests signed by the agent are rejected. Requires
    /// API-key credentials (see [`Config::api_key`](crate::Config::api_key)). As
    /// with [`Client::fetch_agents`], the SDK signs with whatever credential is
    /// configured and does not enforce the scheme per endpoint.
    pub async fn revoke_agent(&self, address: &str) -> Result<serde_json::Value> {
        let addr = encoded_segment(address, "address")?;
        self.signed_delete(&format!("/agents/{addr}")).await
    }

    /// Account balance and collateral summary. Requires credentials.
    pub async fn fetch_balance(&self) -> Result<AccountSummary> {
        self.signed_get("/api/v1/account", &[]).await
    }

    /// Open positions for the authenticated account. Requires credentials.
    ///
    /// Each [`Position`] carries the enriched per-position risk detail
    /// (leverage, notional value, ROE, margin used, max leverage, funding paid);
    /// see [`Position`] for why a risk field can be `None` and why its
    /// companion `*_error` matters.
    pub async fn fetch_positions(&self) -> Result<Vec<Position>> {
        self.signed_get("/api/v1/positions", &[]).await
    }

    /// Closed positions for the authenticated account, newest first
    /// (`GET /api/v1/positions/closed`). Requires credentials.
    ///
    /// The realized counterpart of [`fetch_positions`](Self::fetch_positions):
    /// [`ClosedPosition`] carries the size and prices at close plus the PnL the
    /// close booked, rather than live risk detail.
    ///
    /// Returns the **first page only**. `limit` is that page's size and must be in
    /// `1..=`[`MAX_CLOSED_POSITIONS_LIMIT`] (200 — the smallest of the paginated
    /// maxima); pass `None` for the server's default of 100. Out-of-range values
    /// are rejected here, before the request is signed or sent. Use
    /// [`fetch_closed_positions_paginated`](Self::fetch_closed_positions_paginated)
    /// for the whole history.
    pub async fn fetch_closed_positions(&self, limit: Option<u32>) -> Result<Vec<ClosedPosition>> {
        check_page_size(limit, MAX_CLOSED_POSITIONS_LIMIT, "positions/closed")?;
        self.signed_get("/api/v1/positions/closed", &limit_query(limit))
            .await
    }

    /// Every closed position on the authenticated account, as an auto-paging
    /// [`Paginator`] that follows the `X-Next-Cursor` header for you. Requires
    /// credentials.
    ///
    /// Each page costs one signed request, issued lazily — the cursor rides in the
    /// query string, so every page is independently signed.
    ///
    /// ```no_run
    /// # use nexus_exchange::{Client, Result};
    /// # async fn run(client: &Client) -> Result<()> {
    /// let mut pager = client.fetch_closed_positions_paginated().page_size(200);
    /// while let Some(page) = pager.next_page().await? {
    ///     let _ = (page.items, page.next_cursor);
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// [`page_size`](Paginator::page_size) must be in
    /// `1..=`[`MAX_CLOSED_POSITIONS_LIMIT`] (**200**, the tightest bound of the
    /// five paginated endpoints — a size valid on `/orders/history` is not valid
    /// here); an out-of-range value fails on the first page fetch, before anything
    /// is signed or sent.
    pub fn fetch_closed_positions_paginated(&self) -> Paginator<ClosedPosition> {
        let client = self.clone();
        Paginator::new(move |req: PageRequest| {
            let client = client.clone();
            async move {
                check_page_size(req.limit, MAX_CLOSED_POSITIONS_LIMIT, "positions/closed")?;
                let (items, next) = client
                    .signed_get_page::<Vec<ClosedPosition>>(
                        "/api/v1/positions/closed",
                        &page_query(&req),
                    )
                    .await?;
                Ok(Page::new(items, next))
            }
        })
    }

    /// Aggregate portfolio summary for the authenticated account
    /// (`GET /api/v1/account/summary`) — equity, PnL, volume, open counts, and
    /// [`withdrawable`](AccountPortfolioSummary::withdrawable). Requires
    /// credentials.
    ///
    /// To get this together with the account's positions from a single coherent
    /// read, use [`fetch_account_state`](Self::fetch_account_state) instead.
    ///
    /// # Fails closed, so an `Err` is not an empty account
    ///
    /// This endpoint derives `withdrawable` from the engine-authoritative margin
    /// view, and returns `502` with
    /// [`code`](Error::code) `authoritative_margin_unavailable` when that view is
    /// temporarily down rather than reporting a locally-estimated figure. Retry
    /// after a short delay; **do not** read the error as a flat or zero-balance
    /// account.
    pub async fn fetch_account_summary(&self) -> Result<AccountPortfolioSummary> {
        self.signed_get("/api/v1/account/summary", &[]).await
    }

    /// Consolidated account snapshot (`GET /api/v1/account/state`) — the
    /// portfolio summary **and** every open position from one server-side read.
    /// Requires credentials.
    ///
    /// Prefer this over calling
    /// [`fetch_account_summary`](Self::fetch_account_summary) and
    /// [`fetch_positions`](Self::fetch_positions) separately: those are two
    /// independent requests, so a fill landing between them returns an
    /// internally inconsistent pair (an aggregate that disagrees with the
    /// position list). Here both halves are guaranteed consistent — see
    /// [`AccountState`].
    ///
    /// # Fails closed, so an `Err` is not an empty account
    ///
    /// Like [`fetch_account_summary`](Self::fetch_account_summary), this returns
    /// `502` with [`code`](Error::code) `authoritative_margin_unavailable` when
    /// the engine-authoritative margin view is temporarily unavailable, rather
    /// than serving a locally-estimated balance. Retry after a short delay;
    /// **do not** read the error as an account with no positions.
    pub async fn fetch_account_state(&self) -> Result<AccountState> {
        self.signed_get("/api/v1/account/state", &[]).await
    }

    /// The authenticated account's effective fee schedule
    /// (`GET /api/v1/account/fees`). Requires credentials.
    ///
    /// Returns the forward-looking schedule rate, not a realized per-fill
    /// average. Note [`AccountFees::maker_fee_bps`] is signed — a negative value
    /// is a maker *rebate* — and [`AccountFees::schedule`] scopes which
    /// per-market schedule the rate belongs to.
    pub async fn fetch_account_fees(&self) -> Result<AccountFees> {
        self.signed_get("/api/v1/account/fees", &[]).await
    }

    /// Portfolio time series for the authenticated account
    /// (`GET /api/v1/account/portfolio-history`) — equity, cumulative PnL, and
    /// cumulative volume, oldest first. Requires credentials.
    ///
    /// `window` selects the span *and* the server-side downsample cadence and
    /// point capacity (see [`PortfolioWindow`]); `None` takes the server's `day`
    /// default. Read the served window back from
    /// [`PortfolioHistory::window`] — an open string, so a window added to a later
    /// spec still decodes — rather than assuming the requested value.
    ///
    /// `limit` caps the number of points returned; pass `None` for the full
    /// window. The spec's request schema is `minimum: 1, maximum: 366`, and both
    /// bounds are enforced locally — before the request is signed or sent — so a
    /// value the schema forbids is never transmitted. The parameter's prose notes
    /// that the server *clamps* an over-capacity value rather than rejecting it,
    /// but that describes server tolerance of non-conforming input, not licence
    /// for a client to exceed the schema; the sibling Python SDK and MCP server
    /// bound it the same way.
    ///
    /// `limit` is only an upper bound in either direction: the server still
    /// clamps to the selected window's own capacity (day 288, week 168, month
    /// 120, all 366), which is below `366` for every window but
    /// [`All`](PortfolioWindow::All). Read
    /// [`points`](PortfolioHistory::points)`.len()` rather than assuming the
    /// requested count.
    pub async fn fetch_portfolio_history(
        &self,
        window: Option<PortfolioWindow>,
        limit: Option<u32>,
    ) -> Result<PortfolioHistory> {
        let mut query = Vec::new();
        if let Some(window) = window {
            query.push(("window", window.as_str().to_string()));
        }
        if let Some(limit) = limit {
            if limit == 0 || limit > MAX_PORTFOLIO_HISTORY_LIMIT {
                return Err(Error::invalid_request(format!(
                    "limit must be between 1 and {MAX_PORTFOLIO_HISTORY_LIMIT}"
                )));
            }
            query.push(("limit", limit.to_string()));
        }
        self.signed_get("/api/v1/account/portfolio-history", &query)
            .await
    }

    /// Account equity time series, **oldest first**
    /// (`GET /api/v1/account/equity-history`). Requires credentials.
    ///
    /// The high-resolution recent view — 5s cadence over roughly one hour — where
    /// [`fetch_portfolio_history`](Self::fetch_portfolio_history) is the
    /// downsampled long-window one. Note [`EquityPoint::equity`] arrives as a JSON
    /// *number*, unlike [`PortfolioPoint`](crate::types::PortfolioPoint)'s decimal
    /// string; see [`EquityPoint`] before using it for anything authoritative.
    ///
    /// Returns the **first page only** — which here is normally the entire series:
    /// `limit` must be in `1..=`[`MAX_EQUITY_HISTORY_LIMIT`] and **720 is also this
    /// endpoint's default**, so `None` asks for the whole window rather than the
    /// 100 the other paginated endpoints default to. Out-of-range values are
    /// rejected before the request is signed or sent. Use
    /// [`fetch_equity_history_paginated`](Self::fetch_equity_history_paginated) if
    /// the series is longer than one page.
    pub async fn fetch_equity_history(&self, limit: Option<u32>) -> Result<Vec<EquityPoint>> {
        check_page_size(limit, MAX_EQUITY_HISTORY_LIMIT, "account/equity-history")?;
        self.signed_get("/api/v1/account/equity-history", &limit_query(limit))
            .await
    }

    /// Every available equity sample for the authenticated account, as an
    /// auto-paging [`Paginator`] that follows the `X-Next-Cursor` header for you.
    /// Requires credentials.
    ///
    /// Each page costs one signed request, issued lazily — the cursor rides in the
    /// query string, so every page is independently signed.
    ///
    /// ```no_run
    /// # use nexus_exchange::{Client, Result};
    /// # async fn run(client: &Client) -> Result<()> {
    /// let equity = client.fetch_equity_history_paginated().all().await?;
    /// # let _ = equity;
    /// # Ok(()) }
    /// ```
    ///
    /// [`page_size`](Paginator::page_size) must be in
    /// `1..=`[`MAX_EQUITY_HISTORY_LIMIT`] (720); an out-of-range value fails on the
    /// first page fetch, before anything is signed or sent. Leaving it unset sends
    /// no `limit`, which this endpoint reads as its default of 720 — so the first
    /// page is usually the last.
    pub fn fetch_equity_history_paginated(&self) -> Paginator<EquityPoint> {
        let client = self.clone();
        Paginator::new(move |req: PageRequest| {
            let client = client.clone();
            async move {
                check_page_size(
                    req.limit,
                    MAX_EQUITY_HISTORY_LIMIT,
                    "account/equity-history",
                )?;
                let (items, next) = client
                    .signed_get_page::<Vec<EquityPoint>>(
                        "/api/v1/account/equity-history",
                        &page_query(&req),
                    )
                    .await?;
                Ok(Page::new(items, next))
            }
        })
    }

    /// Recent fills (private trade executions) for the authenticated account
    /// (`GET /api/v1/fills`). Requires credentials.
    ///
    /// Returns the **first page only**. `limit` is that page's size and must be in
    /// `1..=`[`MAX_FILLS_LIMIT`] (1000); pass `None` for the server's default of
    /// 100. Out-of-range values are rejected here, before the request is signed or
    /// sent. Use
    /// [`fetch_my_trades_paginated`](Self::fetch_my_trades_paginated) to walk the
    /// account's whole fill history.
    ///
    /// # Breaking change
    ///
    /// `limit` is new: v0.7.2 documents it on `/fills` and this method sent none at
    /// all, so a single call could never read more than the server's default 100
    /// fills. Callers that want the old behaviour pass `None`:
    ///
    /// ```text
    /// client.fetch_my_trades()       ->  client.fetch_my_trades(None)
    /// ```
    ///
    /// The signature now matches every other flat list read in this SDK
    /// ([`fetch_trades`](Self::fetch_trades),
    /// [`fetch_ohlcv`](Self::fetch_ohlcv),
    /// [`fetch_funding_rate_history`](Self::fetch_funding_rate_history),
    /// [`fetch_portfolio_history`](Self::fetch_portfolio_history)) and the sibling
    /// Python SDK's `fetch_my_trades(limit=...)`.
    pub async fn fetch_my_trades(&self, limit: Option<u32>) -> Result<Vec<Fill>> {
        check_page_size(limit, MAX_FILLS_LIMIT, "fills")?;
        // Built inline rather than through a shared helper so this change stays
        // independent of the history-endpoints PR (ENG-8148), which adds one.
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        self.signed_get("/api/v1/fills", &query).await
    }

    /// Every fill on the authenticated account, as an auto-paging [`Paginator`]
    /// that follows the `X-Next-Cursor` header for you. Requires credentials.
    ///
    /// Each page costs one signed request, issued lazily — the cursor rides in
    /// the query string, so every page is independently signed.
    ///
    /// ```no_run
    /// # use nexus_exchange::{Client, Result};
    /// # async fn run(client: &Client) -> Result<()> {
    /// let fills = client.fetch_my_trades_paginated().page_size(1000).all().await?;
    ///
    /// // Resume a backfill from a cursor persisted on a previous run.
    /// let mut pager = client.fetch_my_trades_paginated().starting_after("saved-cursor");
    /// while let Some(page) = pager.next_page().await? {
    ///     let _ = (page.items, page.next_cursor);
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// [`page_size`](Paginator::page_size) sets the per-page `limit` and must be
    /// in `1..=`[`MAX_FILLS_LIMIT`]; an out-of-range value fails on the first page
    /// fetch, before anything is signed or sent.
    ///
    /// Nothing bounds how far back the walk goes — pass
    /// [`max_pages`](Paginator::max_pages) on an account with a long history.
    pub fn fetch_my_trades_paginated(&self) -> Paginator<Fill> {
        let client = self.clone();
        Paginator::new(move |req: PageRequest| {
            let client = client.clone();
            async move {
                check_page_size(req.limit, MAX_FILLS_LIMIT, "fills")?;
                let (items, next) = client
                    .signed_get_page::<Vec<Fill>>("/api/v1/fills", &page_query(&req))
                    .await?;
                Ok(Page::new(items, next))
            }
        })
    }

    /// Place a single order. Requires credentials.
    pub async fn create_order(&self, order: &OrderRequest) -> Result<OrderResponse> {
        self.signed_post("/api/v1/orders", order).await
    }

    /// Project an order's margin / equity / fee impact **without submitting it**
    /// (`POST /api/v1/orders/preview`). Requires credentials.
    ///
    /// Takes the same [`OrderRequest`] as [`create_order`](Self::create_order),
    /// so the preview-then-commit flow reuses one value: build it, preview it,
    /// and pass the *same* request to `create_order` if the projection is
    /// acceptable. Nothing is placed and no margin is reserved.
    ///
    /// # A rejected preview is `Ok`, not `Err`
    ///
    /// The endpoint's job is to answer "what would this order do?", so a
    /// projection saying the order *would be rejected* is a successful `200`:
    /// [`OrderPreview::is_accepted`] is `false` and
    /// [`OrderPreview::reject_reason`] explains why. Gate submission on
    /// `is_accepted()`, never on `Result::is_ok()`.
    ///
    /// # Errors
    ///
    /// Only genuine request failures are `Err`, classified as everywhere else in
    /// the crate — and each carries the server's machine-readable
    /// [`code`](crate::Error::code):
    ///
    /// | Response | Error |
    /// |---|---|
    /// | `400` validation error | [`TerminalError::BadRequest`], or [`TerminalError::InvalidOrder`] for an engine order-parameter code (`InvalidTickSize`, `InvalidLotSize`, …) |
    /// | `401` | [`TerminalError::Auth`] (`code` = `unauthorized`) |
    /// | `429` | [`TransientError::RateLimited`], honoring `Retry-After` |
    /// | `5xx` | [`TransientError::Unavailable`], preserving `status` and `code` |
    ///
    /// A `400` here means the request was *malformed* (e.g. a limit order with no
    /// price). An order that is well-formed but unaffordable comes back as an
    /// accepted-`false` preview, not a `400`.
    ///
    /// ```no_run
    /// # use nexus_exchange::types::{Decimal, OrderRequest, Side, TimeInForce};
    /// # async fn f(client: &nexus_exchange::Client) -> nexus_exchange::Result<()> {
    /// let order = OrderRequest::market("BTC-USDX-PERP", Side::Buy, Decimal::ONE);
    /// let preview = client.preview_order(&order).await?;
    /// if !preview.is_accepted() {
    ///     eprintln!("would be rejected: {:?}", preview.reject_reason);
    ///     return Ok(());
    /// }
    /// println!("margin required: {:?}", preview.required_initial_margin);
    /// client.create_order(&order).await?;
    /// # Ok(()) }
    /// ```
    ///
    /// [`TerminalError::BadRequest`]: crate::TerminalError::BadRequest
    /// [`TerminalError::InvalidOrder`]: crate::TerminalError::InvalidOrder
    /// [`TerminalError::Auth`]: crate::TerminalError::Auth
    /// [`TransientError::RateLimited`]: crate::TransientError::RateLimited
    /// [`TransientError::Unavailable`]: crate::TransientError::Unavailable
    pub async fn preview_order(&self, order: &OrderRequest) -> Result<OrderPreview> {
        self.signed_post("/api/v1/orders/preview", order).await
    }

    /// Submit a batch of orders (`POST /api/v1/orders/batch`). Requires
    /// credentials.
    ///
    /// Orders are processed sequentially and non-atomically: an early order
    /// consuming margin can reject a later one, and a per-order rejection does
    /// not abort the batch. The returned [`OrderResult`] array preserves request
    /// order, with each entry independently reporting a placed order or a
    /// per-order rejection — match the variant (or use
    /// [`OrderResult::succeeded`]) per entry rather than assuming the whole
    /// batch succeeded.
    pub async fn create_orders(&self, orders: &[OrderRequest]) -> Result<Vec<OrderResult>> {
        self.signed_post("/api/v1/orders/batch", &orders).await
    }

    /// Cancel a single order by id on `market_id`. Requires credentials.
    ///
    /// `market_id` is required: the engine routes single-order-by-id requests to
    /// the order's owning market, so a missing or wrong market resolves to
    /// `OrderNotFound`. It is sent as the `?market_id=` query and rejected
    /// locally if empty.
    pub async fn cancel_order(&self, order_id: &str, market_id: &str) -> Result<serde_json::Value> {
        require_non_empty(market_id, "market_id")?;
        self.signed_delete_with_query(
            &format!("/api/v1/orders/{order_id}"),
            &[("market_id", market_id.to_string())],
        )
        .await
    }

    /// Cancel all open orders for the account. Requires credentials.
    ///
    /// To flatten a single market instead, use
    /// [`cancel_orders_for_market`](Self::cancel_orders_for_market) — it saves
    /// the `fetch_open_orders` → filter → `cancel_orders` round-trip on the
    /// hot reprice path.
    pub async fn cancel_all_orders(&self) -> Result<serde_json::Value> {
        self.signed_delete("/api/v1/orders").await
    }

    /// Cancel all open orders for a single market
    /// (`DELETE /api/v1/orders?market_id=`). Requires credentials.
    ///
    /// Maps to the per-market reprice loop of a market maker quoting many
    /// markets: flatten one market in a single round-trip rather than fetching
    /// open orders, filtering client-side, and cancelling by id.
    ///
    /// An empty `market_id` is rejected locally and never sent: omitting the
    /// filter on `DELETE /api/v1/orders` cancels account-wide, so a blank market must
    /// not be allowed to silently widen a per-market cancel into a full
    /// account flatten. Use [`cancel_all_orders`](Self::cancel_all_orders)
    /// when that account-wide cancel is what you actually want.
    pub async fn cancel_orders_for_market(&self, market_id: &str) -> Result<serde_json::Value> {
        require_non_empty(market_id, "market_id")?;
        self.signed_delete_with_query("/api/v1/orders", &[("market_id", market_id.to_string())])
            .await
    }

    /// List open orders for the authenticated account. Requires credentials.
    ///
    /// For orders that have already finished, see
    /// [`fetch_order_history`](Self::fetch_order_history).
    pub async fn fetch_open_orders(&self) -> Result<Vec<Order>> {
        self.signed_get("/api/v1/orders", &[]).await
    }

    /// Terminal-status order history — filled / cancelled / rejected / expired
    /// orders for the authenticated account, newest first
    /// (`GET /api/v1/orders/history`). Requires credentials.
    ///
    /// The history counterpart of
    /// [`fetch_open_orders`](Self::fetch_open_orders), returning
    /// [`OrderHistoryEntry`] rather than [`Order`].
    ///
    /// Returns the **first page only**. `limit` is that page's size and must be in
    /// `1..=`[`MAX_ORDER_HISTORY_LIMIT`] (500); pass `None` for the server's
    /// default of 100. Out-of-range values are rejected here, before the request is
    /// signed or sent. Use
    /// [`fetch_order_history_paginated`](Self::fetch_order_history_paginated) to
    /// walk the whole history.
    pub async fn fetch_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderHistoryEntry>> {
        check_page_size(limit, MAX_ORDER_HISTORY_LIMIT, "orders/history")?;
        self.signed_get("/api/v1/orders/history", &limit_query(limit))
            .await
    }

    /// Every terminal-status order on the authenticated account, as an auto-paging
    /// [`Paginator`] that follows the `X-Next-Cursor` header for you. Requires
    /// credentials.
    ///
    /// Each page costs one signed request, issued lazily — the cursor rides in the
    /// query string, so every page is independently signed.
    ///
    /// ```no_run
    /// # use nexus_exchange::{Client, Result};
    /// # async fn run(client: &Client) -> Result<()> {
    /// let history = client
    ///     .fetch_order_history_paginated()
    ///     .page_size(500)
    ///     .max_pages(20)
    ///     .all()
    ///     .await?;
    /// # let _ = history;
    /// # Ok(()) }
    /// ```
    ///
    /// [`page_size`](Paginator::page_size) must be in
    /// `1..=`[`MAX_ORDER_HISTORY_LIMIT`] (500 — *not* the 1000 that `/fills` and
    /// the public trades feed allow); an out-of-range value fails on the first
    /// page fetch, before anything is signed or sent.
    ///
    /// Nothing bounds how far back the walk goes — pass
    /// [`max_pages`](Paginator::max_pages) on an account with a long trading
    /// history.
    pub fn fetch_order_history_paginated(&self) -> Paginator<OrderHistoryEntry> {
        let client = self.clone();
        Paginator::new(move |req: PageRequest| {
            let client = client.clone();
            async move {
                check_page_size(req.limit, MAX_ORDER_HISTORY_LIMIT, "orders/history")?;
                let (items, next) = client
                    .signed_get_page::<Vec<OrderHistoryEntry>>(
                        "/api/v1/orders/history",
                        &page_query(&req),
                    )
                    .await?;
                Ok(Page::new(items, next))
            }
        })
    }

    /// Fetch a single order by id on `market_id`. Requires credentials.
    ///
    /// `market_id` is required: the engine routes single-order-by-id requests to
    /// the order's owning market, so a missing or wrong market resolves to
    /// `OrderNotFound`. It is sent as the `?market_id=` query and rejected
    /// locally if empty.
    pub async fn fetch_order(&self, order_id: &str, market_id: &str) -> Result<Order> {
        require_non_empty(market_id, "market_id")?;
        self.signed_get(
            &format!("/orders/{order_id}"),
            &[("market_id", market_id.to_string())],
        )
        .await
    }

    /// Deposit **real** USDX collateral (`POST /account/deposit`). Requires
    /// credentials.
    ///
    /// This moves real funds and is the production funding path. To fund a
    /// non-production (testnet) account, use the faucet
    /// ([`claim_credit`](Self::claim_credit)) — or the network-aware
    /// [`fund`](Self::fund) convenience, which routes to the right primitive.
    /// A non-positive amount is rejected locally before sending.
    pub async fn deposit(&self, amount: Decimal) -> Result<DepositResult> {
        if amount <= Decimal::ZERO {
            return Err(Error::invalid_request("deposit amount must be positive"));
        }
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
    /// allowance (`POST /account/credit`). Omit `amount` to claim the full
    /// remaining allowance. Requires credentials.
    ///
    /// This is the non-production funding path; the production counterpart that
    /// moves real collateral is [`deposit`](Self::deposit). To pick between the
    /// two by network automatically, see [`fund`](Self::fund).
    pub async fn claim_credit(&self, amount: Option<Decimal>) -> Result<CreditResult> {
        let body = match amount {
            Some(a) => serde_json::json!({ "amount": a.to_string() }),
            None => serde_json::json!({}),
        };
        self.signed_post("/api/v1/account/credit", &body).await
    }

    /// Network-aware funding convenience: fund the account with `amount` USDX
    /// using the primitive that fits the configured [`Network`](crate::Network), so callers
    /// don't have to remember which of [`deposit`](Self::deposit) (real
    /// collateral) vs [`claim_credit`](Self::claim_credit) (testnet faucet)
    /// applies. Requires credentials.
    ///
    /// Routing:
    /// - **Non-production** network ([`Network::is_production`](crate::Network::is_production) is `false`,
    ///   i.e. [`Beta`](crate::Network::Beta) / [`Local`](crate::Network::Local)):
    ///   claims `amount` from the testnet faucet ([`claim_credit`](Self::claim_credit)).
    /// - **Production** ([`Network::Stable`](crate::Network::Stable)): rejected
    ///   locally. `fund` will **never silently move real collateral** — depositing
    ///   real funds must be an explicit, deliberate [`deposit`](Self::deposit)
    ///   call, not a side effect of a convenience helper.
    /// - **Unknown** network (client built with [`Config::with_base_url`](crate::Config::with_base_url),
    ///   so the host's real-money character is unknown): rejected locally; call
    ///   [`deposit`](Self::deposit) or [`claim_credit`](Self::claim_credit)
    ///   explicitly.
    ///
    /// A non-positive `amount` is rejected locally. All rejections happen before
    /// any request is sent.
    pub async fn fund(&self, amount: Decimal) -> Result<CreditResult> {
        if amount <= Decimal::ZERO {
            return Err(Error::invalid_request("fund amount must be positive"));
        }
        match self.config.network {
            Some(network) if !network.is_production() => self.claim_credit(Some(amount)).await,
            Some(_) => Err(Error::invalid_request(
                "fund() claims synthetic testnet credit and refuses to move real \
                 collateral on a production network; call deposit() explicitly to \
                 deposit real USDX",
            )),
            None => Err(Error::invalid_request(
                "fund() needs a known Network to choose a funding primitive, but this \
                 client was built with a custom base URL; call claim_credit() (testnet \
                 faucet) or deposit() (real collateral) explicitly",
            )),
        }
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

    // --- Tier 3: leverage / margin, order amend, batch, client order ids,
    // funding & transfer history, sub-accounts. ---

    /// Set the leverage used for a market (`POST /account/leverage`). Requires
    /// credentials.
    ///
    /// `leverage` is the integer multiplier (e.g. `10` for 10×). Must be at
    /// least 1 — that's checked locally before sending; the market's actual
    /// ceiling ([`Market::max_leverage`](crate::types::Market::max_leverage)) is
    /// enforced server-side.
    pub async fn set_leverage(&self, market_id: &str, leverage: u32) -> Result<LeverageUpdate> {
        require_non_empty(market_id, "market_id")?;
        if leverage == 0 {
            return Err(Error::invalid_request("leverage must be at least 1"));
        }
        self.signed_post(
            "/account/leverage",
            &serde_json::json!({ "market_id": market_id, "leverage": leverage }),
        )
        .await
    }

    /// Set the margin mode (cross or isolated) for a market
    /// (`POST /account/margin-mode`). Requires credentials.
    pub async fn set_margin_mode(
        &self,
        market_id: &str,
        margin_mode: MarginMode,
    ) -> Result<MarginModeUpdate> {
        require_non_empty(market_id, "market_id")?;
        self.signed_post(
            "/account/margin-mode",
            &serde_json::json!({ "market_id": market_id, "margin_mode": margin_mode }),
        )
        .await
    }

    /// Add or remove isolated margin on an open position (`POST /account/margin`).
    /// Requires credentials.
    ///
    /// Only applies to a position in [`MarginMode::Isolated`] mode — the server
    /// rejects a cross-margined position with `MarginModeNotIsolated`. `amount`
    /// is the collateral to move, sent as a decimal string; it must be positive
    /// (checked locally before sending). Removing more than the position's free
    /// isolated margin, or below the withdrawal floor, is rejected server-side
    /// (`InsufficientMargin` / `InsufficientBalance`); a market with no open
    /// position yields `NoOpenPosition`.
    ///
    /// See also [`add_margin`](Self::add_margin) and
    /// [`remove_margin`](Self::remove_margin), thin wrappers that fix the
    /// direction.
    pub async fn adjust_margin(
        &self,
        market_id: &str,
        direction: MarginDirection,
        amount: Decimal,
    ) -> Result<MarginAdjustment> {
        require_non_empty(market_id, "market_id")?;
        if amount <= Decimal::ZERO {
            return Err(Error::invalid_request("margin amount must be positive"));
        }
        self.signed_post(
            "/account/margin",
            &serde_json::json!({
                "market_id": market_id,
                "direction": direction,
                "amount": amount.to_string(),
            }),
        )
        .await
    }

    /// Add isolated margin to an open position (`POST /account/margin` with
    /// `direction: add`). Requires credentials. Convenience wrapper over
    /// [`adjust_margin`](Self::adjust_margin).
    pub async fn add_margin(&self, market_id: &str, amount: Decimal) -> Result<MarginAdjustment> {
        self.adjust_margin(market_id, MarginDirection::Add, amount)
            .await
    }

    /// Remove isolated margin from an open position (`POST /account/margin` with
    /// `direction: remove`). Requires credentials. Convenience wrapper over
    /// [`adjust_margin`](Self::adjust_margin).
    pub async fn remove_margin(
        &self,
        market_id: &str,
        amount: Decimal,
    ) -> Result<MarginAdjustment> {
        self.adjust_margin(market_id, MarginDirection::Remove, amount)
            .await
    }

    /// Amend an open order in place on `market_id` (`PATCH /orders/{id}`) — an
    /// atomic server-side cancel-replace. Requires credentials.
    ///
    /// `market_id` is required: the engine routes single-order-by-id requests to
    /// the order's owning market, so a missing or wrong market resolves to
    /// `OrderNotFound`. It is sent as the `?market_id=` query and rejected
    /// locally if empty.
    ///
    /// Only the fields set on `amend` change; the rest of the order is left as
    /// is. An amend that would change nothing is rejected locally (no request is
    /// sent) so a stray no-op can't silently churn the order's queue priority.
    /// A successful PATCH returns the replacement [`Order`] directly (unlike
    /// `POST /orders`, which wraps its order and fills in [`OrderResponse`]).
    pub async fn amend_order(
        &self,
        order_id: &str,
        market_id: &str,
        amend: &AmendOrder,
    ) -> Result<Order> {
        require_non_empty(market_id, "market_id")?;
        if !amend.has_changes() {
            return Err(Error::invalid_request(
                "amend_order requires at least one field to change",
            ));
        }
        let id = encoded_segment(order_id, "order_id")?;
        self.signed_patch_with_query(
            &format!("/orders/{id}"),
            &[("market_id", market_id.to_string())],
            amend,
        )
        .await
    }

    /// Cancel a batch of orders by id (`POST /orders/batch-cancel`). Requires
    /// credentials. Sequential and non-atomic, like
    /// [`create_orders`](Self::create_orders). The response is left untyped: this
    /// endpoint is ahead of the pinned spec and returns a different shape from
    /// the create batch (a cancellation summary, not a per-order result array),
    /// so it is not modeled by [`OrderResult`]. An empty batch is rejected
    /// locally.
    pub async fn cancel_orders(&self, order_ids: &[&str]) -> Result<serde_json::Value> {
        if order_ids.is_empty() {
            return Err(Error::invalid_request(
                "cancel_orders requires at least one order id",
            ));
        }
        self.signed_post(
            "/orders/batch-cancel",
            &serde_json::json!({ "order_ids": order_ids }),
        )
        .await
    }

    /// Fetch a single order by its caller-assigned client order id
    /// (`GET /orders/by-client-id/{client_order_id}`). Requires credentials.
    pub async fn fetch_order_by_client_id(&self, client_order_id: &str) -> Result<Order> {
        let id = encoded_segment(client_order_id, "client_order_id")?;
        self.signed_get(&format!("/orders/by-client-id/{id}"), &[])
            .await
    }

    /// Cancel a single order by its caller-assigned client order id
    /// (`DELETE /orders/by-client-id/{client_order_id}`). Requires credentials.
    pub async fn cancel_order_by_client_id(
        &self,
        client_order_id: &str,
    ) -> Result<serde_json::Value> {
        let id = encoded_segment(client_order_id, "client_order_id")?;
        self.signed_delete(&format!("/orders/by-client-id/{id}"))
            .await
    }

    /// Funding-payment history for the authenticated account
    /// (`GET /funding-payments`), optionally filtered to a single market.
    /// Requires credentials.
    pub async fn fetch_funding_payments(
        &self,
        market_id: Option<&str>,
    ) -> Result<Vec<FundingPayment>> {
        let mut query = Vec::new();
        if let Some(market_id) = market_id {
            query.push(("market_id", market_id.to_string()));
        }
        self.signed_get("/funding-payments", &query).await
    }

    /// Move collateral between accounts (`POST /transfers`), e.g. to or from a
    /// sub-account. Requires credentials. A non-positive amount is rejected
    /// locally before sending.
    pub async fn create_transfer(&self, transfer: &TransferRequest) -> Result<Transfer> {
        if transfer.amount <= Decimal::ZERO {
            return Err(Error::invalid_request("transfer amount must be positive"));
        }
        self.signed_post("/transfers", transfer).await
    }

    /// Collateral-transfer history for the authenticated account
    /// (`GET /transfers`). Requires credentials.
    pub async fn fetch_transfers(&self) -> Result<Vec<Transfer>> {
        self.signed_get("/transfers", &[]).await
    }

    /// List the sub-accounts of the authenticated master account
    /// (`GET /sub-accounts`). Requires credentials.
    pub async fn fetch_sub_accounts(&self) -> Result<Vec<SubAccount>> {
        self.signed_get("/sub-accounts", &[]).await
    }

    /// Create a new sub-account with the given label (`POST /sub-accounts`).
    /// Requires credentials.
    pub async fn create_sub_account(&self, label: &str) -> Result<SubAccount> {
        self.signed_post("/sub-accounts", &serde_json::json!({ "label": label }))
            .await
    }

    // --- Wallet-signed auth flows (EIP-191 / EIP-712) ---

    /// EIP-191 session login (`POST /auth/login`). Signs the fixed login
    /// message with `signer` and exchanges it for a 24-hour session token.
    ///
    /// Unauthenticated — the signature *is* the authorization. This is a thin
    /// signer: it returns the [`LoginResponse`] and does not store or refresh
    /// the token. To use it for `/keys` management, pass
    /// [`LoginResponse::token`] to [`Config::session_token`](crate::Config::session_token).
    pub async fn sign_in(&self, signer: &EthSigner) -> Result<LoginResponse> {
        let body = signer.sign_in()?;
        self.post_unsigned("/auth/login", &body).await
    }

    /// EIP-712 agent-key registration (`POST /agents/register`). Authorizes an
    /// agent keypair to sign trading requests on the wallet's behalf.
    ///
    /// Build the signed [`AgentRegistration`] with
    /// [`EthSigner::register_agent`]. Unauthenticated — the EIP-712 signature
    /// from the owning wallet is the authorization; no session token is needed.
    pub async fn register_agent(
        &self,
        registration: &AgentRegistration,
    ) -> Result<AgentRegistered> {
        self.post_unsigned("/agents/register", registration).await
    }

    // --- Cancel-on-disconnect (COD) --------------------------------------------

    /// Fetch the account's cancel-on-disconnect status
    /// (`GET /api/v1/account/cancel-on-disconnect`). Requires credentials.
    ///
    /// When enabled and active, the exchange cancels the account's resting orders
    /// if the `/ws` connection drops and is not re-established within the grace
    /// window (see [`CancelOnDisconnectStatus`]).
    pub async fn fetch_cancel_on_disconnect(&self) -> Result<CancelOnDisconnectStatus> {
        self.signed_get("/api/v1/account/cancel-on-disconnect", &[])
            .await
    }

    /// Enable or disable cancel-on-disconnect for the account
    /// (`PUT /api/v1/account/cancel-on-disconnect`). Requires credentials.
    ///
    /// Returns the resulting [`CancelOnDisconnectStatus`]; note `active` may stay
    /// false if the exchange has the feature switched off deployment-wide even
    /// when `enabled` is set true.
    pub async fn set_cancel_on_disconnect(
        &self,
        enabled: bool,
    ) -> Result<CancelOnDisconnectStatus> {
        self.signed_put(
            "/api/v1/account/cancel-on-disconnect",
            &serde_json::json!({ "enabled": enabled }),
        )
        .await
    }

    // --- Bridge (Phase A: deposits) --------------------------------------------

    /// List the supported bridge chains and their deposit/withdraw assets
    /// (`GET /api/v1/bridge/assets`). Unauthenticated.
    pub async fn fetch_bridge_assets(&self) -> Result<BridgeAssetsResponse> {
        self.get("/api/v1/bridge/assets", &[], COST_DEFAULT).await
    }

    /// Get-or-create the account's deposit address on `chain`
    /// (`POST /api/v1/bridge/deposit-addresses`). Requires credentials.
    ///
    /// Idempotent per `(account, chain)`: repeated calls return the same address.
    pub async fn create_bridge_deposit_address(&self, chain: &str) -> Result<BridgeDepositAddress> {
        require_non_empty(chain, "chain")?;
        self.signed_post(
            "/api/v1/bridge/deposit-addresses",
            &serde_json::json!({ "chain": chain }),
        )
        .await
    }

    /// List the account's bridge deposit addresses
    /// (`GET /api/v1/bridge/deposit-addresses`). Requires credentials.
    pub async fn fetch_bridge_deposit_addresses(&self) -> Result<Vec<BridgeDepositAddress>> {
        self.signed_get("/api/v1/bridge/deposit-addresses", &[])
            .await
    }

    /// List the account's tracked cross-chain deposits
    /// (`GET /api/v1/bridge/deposits`). Requires credentials.
    pub async fn fetch_bridge_deposits(&self) -> Result<Vec<BridgeDeposit>> {
        self.signed_get("/api/v1/bridge/deposits", &[]).await
    }

    /// Fetch a single tracked bridge deposit by id
    /// (`GET /api/v1/bridge/deposits/{id}`). Requires credentials.
    pub async fn fetch_bridge_deposit(&self, id: &str) -> Result<BridgeDeposit> {
        let id = encoded_segment(id, "id")?;
        self.signed_get(&format!("/api/v1/bridge/deposits/{id}"), &[])
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::encode_path_segment;

    #[test]
    fn encode_path_segment_is_noop_for_ids_and_addresses() {
        assert_eq!(encode_path_segment("nx_a1B2-c3~d"), "nx_a1B2-c3~d");
        assert_eq!(
            encode_path_segment("0xAbC0123456789abcdef"),
            "0xAbC0123456789abcdef"
        );
    }

    #[test]
    fn encode_path_segment_neutralizes_injection() {
        // A slash can't graft on extra path / route to a sibling resource, so
        // `..` is confined to a single segment and can't traverse upward.
        assert_eq!(encode_path_segment("../account"), "..%2Faccount");
        // Query and fragment delimiters are escaped, not honored.
        assert_eq!(encode_path_segment("k?a=1"), "k%3Fa%3D1");
        assert_eq!(encode_path_segment("k#frag"), "k%23frag");
    }

    // Routing a non-production `fund()` to the faucet needs both a declared
    // `Network` and a mock-server base URL — a combination the public builders
    // can't express (`with_base_url` carries no network). This in-crate test
    // sets the `pub(crate)` base URL directly to assert the wiring: a
    // non-production fund() POSTs the amount to the credit/faucet endpoint.
    #[tokio::test]
    async fn fund_on_non_production_claims_faucet_credit() {
        use crate::{Client, Config, Network};
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/account/credit"))
            .and(body_json(serde_json::json!({ "amount": "250" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "amount": "250", "credited_today": "250", "daily_limit": "500"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = Config::new(Network::Local).api_key(
            "nx_test",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        );
        // `account/credit` lives on the `/api/v1` surface, which routes to
        // `direct_base_url`; point both bases at the mock.
        config.base_url = server.uri();
        config.direct_base_url = server.uri();
        let r = Client::new(config)
            .fund("250".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(r.amount.to_string(), "250");
    }
}
