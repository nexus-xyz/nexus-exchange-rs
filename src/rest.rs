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

use crate::auth::{AgentRegistration, EthSigner};
use crate::types::{
    AccountSummary, AdlEvent, AgentInfo, AgentRegistered, AmendOrder, ApiKeyInfo, CreatedApiKey,
    CreditResult, Decimal, DepositResult, Fill, FundingPayment, FundingSample, HealthStatus,
    LeverageUpdate, LoginResponse, MarginMode, MarginModeUpdate, MarkPrice, Market, MarketStatus,
    MarketSummary, Ohlcv, Order, OrderBook, OrderRequest, OrderResponse, Position, RateLimitStatus,
    SubAccount, Ticker, TierOverride, Trade, Transfer, TransferRequest, Withdrawal, WsToken,
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
        self.get("/markets/summary", &[], COST_DEFAULT).await
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
    ///
    /// To flatten a single market instead, use
    /// [`cancel_orders_for_market`](Self::cancel_orders_for_market) — it saves
    /// the `fetch_open_orders` → filter → `cancel_orders` round-trip on the
    /// hot reprice path.
    pub async fn cancel_all_orders(&self) -> Result<serde_json::Value> {
        self.signed_delete("/orders").await
    }

    /// Cancel all open orders for a single market (`DELETE /orders?market_id=`).
    /// Requires credentials.
    ///
    /// Maps to the per-market reprice loop of a market maker quoting many
    /// markets: flatten one market in a single round-trip rather than fetching
    /// open orders, filtering client-side, and cancelling by id.
    ///
    /// An empty `market_id` is rejected locally and never sent: omitting the
    /// filter on `DELETE /orders` cancels account-wide, so a blank market must
    /// not be allowed to silently widen a per-market cancel into a full
    /// account flatten. Use [`cancel_all_orders`](Self::cancel_all_orders)
    /// when that account-wide cancel is what you actually want.
    pub async fn cancel_orders_for_market(&self, market_id: &str) -> Result<serde_json::Value> {
        require_non_empty(market_id, "market_id")?;
        self.signed_delete_with_query("/orders", &[("market_id", market_id.to_string())])
            .await
    }

    /// List open orders for the authenticated account. Requires credentials.
    pub async fn fetch_open_orders(&self) -> Result<Vec<Order>> {
        self.signed_get("/orders", &[]).await
    }

    /// Fetch a single order by id. Requires credentials.
    pub async fn fetch_order(&self, order_id: &str) -> Result<Order> {
        self.signed_get(&format!("/orders/{order_id}"), &[]).await
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
        self.signed_post("/account/credit", &body).await
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

    /// Amend an open order in place (`PUT /orders/{id}`) — an atomic server-side
    /// cancel-replace. Requires credentials.
    ///
    /// Only the fields set on `amend` change; the rest of the order is left as
    /// is. An amend that would change nothing is rejected locally (no request is
    /// sent) so a stray no-op can't silently churn the order's queue priority.
    pub async fn amend_order(&self, order_id: &str, amend: &AmendOrder) -> Result<OrderResponse> {
        if !amend.has_changes() {
            return Err(Error::invalid_request(
                "amend_order requires at least one field to change",
            ));
        }
        let id = encoded_segment(order_id, "order_id")?;
        self.signed_put(&format!("/orders/{id}"), amend).await
    }

    /// Cancel a batch of orders by id (`POST /orders/batch-cancel`). Requires
    /// credentials. Sequential and non-atomic, mirroring
    /// [`create_orders`](Self::create_orders); the per-order result array is
    /// currently untyped in the spec. An empty batch is rejected locally.
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
            .and(path("/account/credit"))
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
        config.base_url = server.uri();
        let r = Client::new(config)
            .fund("250".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(r.amount.to_string(), "250");
    }
}
