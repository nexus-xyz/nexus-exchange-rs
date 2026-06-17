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
