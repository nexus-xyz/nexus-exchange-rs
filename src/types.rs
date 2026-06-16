//! Wire types — requests, responses, and shared enums.
//!
//! Money is `rust_decimal::Decimal`. Fields the API sends as decimal *strings*
//! use the `str` serde adapter; fields it sends as JSON *numbers* use the
//! `float` adapter — so callers get one consistent money type regardless of
//! the wire encoding.

use rust_decimal::Decimal;
use serde::Deserialize;
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
    #[serde(with = "rust_decimal::serde::float")]
    pub mark_price: Decimal,
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
    #[serde(with = "rust_decimal::serde::float_option")]
    pub high: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub low: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub bid: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub bid_volume: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub ask: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub ask_volume: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub open: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub close: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub last: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub change: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub percentage: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub base_volume: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub quote_volume: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
    pub mark_price: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::float_option")]
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

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
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
    pub events_received: u64,
    pub fills_total: u64,
    pub uptime_seconds: u64,
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
    #[serde(with = "rust_decimal::serde::str")]
    pub liquidation_price: Decimal,
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
