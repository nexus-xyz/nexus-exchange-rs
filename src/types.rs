//! Wire types — requests, responses, and shared enums.
//!
//! Money is `rust_decimal::Decimal`. Fields the API sends as decimal *strings*
//! use the `str` serde adapter; fields it sends as JSON *numbers* use the
//! `float` adapter — so callers get one consistent money type regardless of
//! the wire encoding.

pub use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tradable market and its trading rules.
#[derive(Debug, Clone, Deserialize)]
pub struct Market {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    pub base_asset: String,
    pub quote_asset: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub tick_size: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub lot_size: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub min_order_size: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub max_order_size: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub initial_margin_rate: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub maintenance_margin_rate: Decimal,
    pub max_leverage: u32,
}

/// Per-market summary with 24h volume and halt state.
#[derive(Debug, Clone, Deserialize)]
pub struct MarketSummary {
    pub market_id: String,
    /// Mark price as a JSON number; `null` for a halted market with no recent
    /// mark (the spec types this `["number","null"]`).
    #[serde(with = "rust_decimal::serde::float_option")]
    pub mark_price: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float")]
    pub volume_24h: Decimal,
    pub trade_count: u64,
    /// Market lifecycle state, e.g. `active`, `halted`.
    pub status: String,
    pub halt_reason: Option<String>,
    /// Unix ms when the market was halted, if it is.
    pub halted_at: Option<i64>,
    pub adl_event_count: u64,
}

/// Market lifecycle / halt status.
#[derive(Debug, Clone, Deserialize)]
pub struct MarketStatus {
    pub market_id: String,
    pub status: String,
    pub halt_reason: Option<String>,
    pub halted_at: Option<i64>,
    pub adl_event_count: u64,
}

/// CCXT-style ticker. Price fields are optional — the API sends `null` when a
/// value is unavailable (e.g. no trades yet).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticker {
    pub symbol: String,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    /// ISO-8601 timestamp.
    pub datetime: String,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub high: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub low: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub bid: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub bid_volume: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub ask: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub ask_volume: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub open: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub close: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub last: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub change: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub percentage: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub base_volume: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub quote_volume: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub mark_price: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub index_price: Option<Decimal>,
    /// Raw exchange-specific payload.
    #[serde(default)]
    pub info: Value,
}

/// A single order-book level, `[price, amount]` (CCXT format).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PriceLevel(
    #[serde(with = "rust_decimal::serde::float")] pub Decimal,
    #[serde(with = "rust_decimal::serde::float")] pub Decimal,
);

impl PriceLevel {
    /// Price at this level.
    pub fn price(&self) -> Decimal {
        self.0
    }
    /// Resting size at this level.
    pub fn amount(&self) -> Decimal {
        self.1
    }
}

/// Order book snapshot. Bids descending, asks ascending (CCXT convention).
#[derive(Debug, Clone, Deserialize)]
pub struct OrderBook {
    pub symbol: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub timestamp: i64,
    pub datetime: String,
    /// Monotonic sequence number for this snapshot.
    pub nonce: i64,
}

/// Order side. Serializes as PascalCase (`Buy`/`Sell`, as order endpoints
/// expect) and deserializes either case (public CCXT feeds use lowercase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Side {
    #[serde(alias = "buy", alias = "BUY")]
    Buy,
    #[serde(alias = "sell", alias = "SELL")]
    Sell,
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum OrderType {
    Limit,
    Market,
}

/// Time-in-force policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TimeInForce {
    /// Good-till-cancelled.
    Gtc,
    /// Immediate-or-cancel.
    Ioc,
    /// Fill-or-kill.
    Fok,
}

/// A public trade print.
#[derive(Debug, Clone, Deserialize)]
pub struct Trade {
    pub id: String,
    pub symbol: String,
    #[serde(with = "rust_decimal::serde::float")]
    pub price: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub amount: Decimal,
    #[serde(with = "rust_decimal::serde::float")]
    pub cost: Decimal,
    pub side: Side,
    pub timestamp: i64,
    pub datetime: String,
    /// `taker` or `maker`, when known.
    #[serde(rename = "takerOrMaker")]
    pub taker_or_maker: Option<String>,
    pub is_liquidation: bool,
    #[serde(default)]
    pub info: Value,
}

/// An OHLCV candle, `[timestamp_ms, open, high, low, close, volume]` (CCXT format).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Ohlcv(
    pub i64,
    #[serde(with = "rust_decimal::serde::float")] pub Decimal,
    #[serde(with = "rust_decimal::serde::float")] pub Decimal,
    #[serde(with = "rust_decimal::serde::float")] pub Decimal,
    #[serde(with = "rust_decimal::serde::float")] pub Decimal,
    #[serde(with = "rust_decimal::serde::float")] pub Decimal,
);

impl Ohlcv {
    /// Open time, Unix ms.
    pub fn timestamp(&self) -> i64 {
        self.0
    }
    pub fn open(&self) -> Decimal {
        self.1
    }
    pub fn high(&self) -> Decimal {
        self.2
    }
    pub fn low(&self) -> Decimal {
        self.3
    }
    pub fn close(&self) -> Decimal {
        self.4
    }
    pub fn volume(&self) -> Decimal {
        self.5
    }
}

/// One intra-hour funding-rate sample.
#[derive(Debug, Clone, Deserialize)]
pub struct FundingSample {
    pub timestamp: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub funding_rate: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub premium_index: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub mark_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub oracle_price: Decimal,
}

/// Current mark price for a market.
#[derive(Debug, Clone, Deserialize)]
pub struct MarkPrice {
    pub market_id: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub mark_price: Decimal,
}

/// Indexer health/status snapshot (`GET /health`). Unknown fields are ignored,
/// so this stays forward-compatible as the snapshot grows.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthStatus {
    #[serde(default)]
    pub events_received: u64,
    #[serde(default)]
    pub fills_total: u64,
    #[serde(default)]
    pub uptime_seconds: u64,
    #[serde(default)]
    pub connected: bool,
    /// Coarse health state, when reported (e.g. `healthy`, `degraded`).
    #[serde(default)]
    pub health: Option<String>,
}

/// An API key associated with the authenticated session (`GET /keys`).
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    pub key_id: String,
    /// Rate-limit tier this key resolves to.
    pub tier: String,
}

/// Account balance and collateral summary (`GET /account`).
#[derive(Debug, Clone, Deserialize)]
pub struct AccountSummary {
    #[serde(with = "rust_decimal::serde::str")]
    pub balance: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub collateral: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub equity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub available_margin: Decimal,
    pub positions: Vec<Position>,
}

/// An open position.
#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    pub market_id: String,
    /// Position direction (e.g. `long`/`short`).
    pub side: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub size: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub entry_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unrealized_pnl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub realized_pnl: Decimal,
    /// Liquidation price. The spec does not mark it required (it can be absent
    /// in flat / cross-margin states), so it's optional rather than hard-failing
    /// the whole balance/positions decode when omitted.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub liquidation_price: Option<Decimal>,
}

/// A fill (private trade execution) for the authenticated account.
#[derive(Debug, Clone, Deserialize)]
pub struct Fill {
    pub id: String,
    pub order_id: String,
    pub market_id: String,
    pub side: Side,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub size: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub fee: Decimal,
    /// `taker` or `maker`, when reported.
    #[serde(default)]
    pub taker_or_maker: Option<String>,
    pub timestamp: i64,
    pub is_liquidation: bool,
}

/// Current rate-limit status (`GET /account/rate-limit`). The numeric fields are
/// `null` for the unlimited tier.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitStatus {
    pub tier: String,
    /// Requests per second ceiling (and burst capacity).
    pub limit: Option<i64>,
    /// Requests available right now.
    pub remaining: Option<i64>,
    /// Unix ms when the bucket refills to `limit`; `0` when already full.
    pub reset_at_ms: Option<i64>,
}

/// A new-order request (`POST /orders`). Construct with [`OrderRequest::limit`]
/// or [`OrderRequest::market`].
#[derive(Debug, Clone, Serialize)]
pub struct OrderRequest {
    pub market_id: String,
    pub side: Side,
    pub order_type: OrderType,
    /// Limit price; omitted for market orders.
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "rust_decimal::serde::str_option"
    )]
    pub price: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    pub time_in_force: TimeInForce,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduce_only: Option<bool>,
}

impl OrderRequest {
    /// A limit order.
    pub fn limit(
        market_id: impl Into<String>,
        side: Side,
        price: Decimal,
        quantity: Decimal,
        time_in_force: TimeInForce,
    ) -> Self {
        Self {
            market_id: market_id.into(),
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity,
            time_in_force,
            reduce_only: None,
        }
    }

    /// A market order (immediate-or-cancel).
    pub fn market(market_id: impl Into<String>, side: Side, quantity: Decimal) -> Self {
        Self {
            market_id: market_id.into(),
            side,
            order_type: OrderType::Market,
            price: None,
            quantity,
            time_in_force: TimeInForce::Ioc,
            reduce_only: None,
        }
    }
}

/// An order record.
#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    pub id: String,
    pub market_id: String,
    pub account_id: String,
    pub side: Side,
    pub order_type: OrderType,
    /// Limit price; `None` for market orders.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub price: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub filled_qty: Decimal,
    /// `Open`, `PartiallyFilled`, `Filled`, `Cancelled`, `Expired`, `Rejected`.
    pub status: String,
    pub time_in_force: TimeInForce,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Response to `POST /orders`: the resulting order plus any immediate fills.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    pub order: Order,
    /// Immediate fills (currently untyped in the spec).
    #[serde(default)]
    pub fills: Vec<serde_json::Value>,
}

/// Result of a deposit (`POST /account/deposit`).
#[derive(Debug, Clone, Deserialize)]
pub struct DepositResult {
    #[serde(with = "rust_decimal::serde::str")]
    pub balance: Decimal,
}

/// A withdrawal record (`GET /withdrawals`).
#[derive(Debug, Clone, Deserialize)]
pub struct Withdrawal {
    pub id: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    pub timestamp: i64,
    pub status: String,
}

/// Result of claiming synthetic USDX credit (`POST /account/credit`).
#[derive(Debug, Clone, Deserialize)]
pub struct CreditResult {
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub credited_today: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub daily_limit: Decimal,
}

/// An account rate-limit tier override (`/admin/tiers`).
#[derive(Debug, Clone, Deserialize)]
pub struct TierOverride {
    pub address: String,
    pub tier: String,
}

/// A freshly minted, single-use WebSocket token (`POST /ws/token`).
#[derive(Debug, Clone, Deserialize)]
pub struct WsToken {
    pub token: String,
}
