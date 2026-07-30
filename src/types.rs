//! Wire types — requests, responses, and shared enums.
//!
//! Money is `rust_decimal::Decimal`. Fields the API sends as decimal *strings*
//! use the `str` serde adapter; fields it sends as JSON *numbers* use the
//! `float` adapter — so callers get one consistent money type regardless of
//! the wire encoding.
//!
//! # Precision of `float`-adapter fields
//!
//! The `str`-adapter fields are **exact**: the wire carries the full decimal
//! text and it is parsed straight into [`Decimal`] with no intermediate type.
//!
//! The `float`-adapter fields are **not guaranteed exact**. That adapter parses
//! the JSON number through an `f64`, and `f64` is binary floating point: most
//! finite decimals (e.g. `0.1`, `123.45`) have no exact `f64` representation, so
//! the resulting [`Decimal`] can carry rounding artifacts — a value sent as
//! `0.1` may decode as `0.1000000000000000055511151231`-style noise rounded to
//! the adapter's precision. `f64` also only holds ~15–17 significant decimal
//! digits, so values with more significant digits than that lose the tail.
//!
//! Practically these artifacts are tiny (sub-`1e-15` relative), but they mean
//! you should **not** treat a `float`-adapter value as an exact ledger figure or
//! compare two of them for bit-exact equality; round to the market's tick/lot
//! size (see [`crate::markets`]) before display or equality checks. Anything
//! authoritative for accounting — balances, fills, order prices, funding — comes
//! from a `str`-adapter field and is exact.
//!
//! The types and fields affected are called out individually below:
//! [`Ticker`], [`Trade`], [`MarketSummary`] (`last_trade_price`, `volume_24h`),
//! [`OrderBook`] / [`PriceLevel`], [`Ohlcv`], [`EquityPoint`] (`equity` — the
//! spec sends this series as JSON numbers, unlike the string-typed
//! [`PortfolioPoint::equity`] derived from the same value), and [`Position`]
//! (`leverage` only — the API sends it as a JSON number; every monetary field on
//! [`Position`], including the enriched risk fields, is a `str`-adapter field
//! and therefore exact).
//!
//! The clean fix is on the API side: if these endpoints emitted decimal strings
//! like the others, the SDK could use the `str` adapter everywhere and every
//! field would be exact. That change is tracked separately; until then this
//! module documents the gap rather than papering over it.

use std::fmt;

pub use rust_decimal::Decimal;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tradable market and its trading rules.
#[derive(Debug, Clone, Deserialize)]
pub struct Market {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Base asset symbol (the asset being traded), e.g. `BTC`.
    pub base_asset: String,
    /// Quote asset symbol (the asset prices are denominated in), e.g. `USDX`.
    pub quote_asset: String,
    /// Smallest permitted price increment. Order prices must be a multiple of this.
    #[serde(with = "rust_decimal::serde::str")]
    pub tick_size: Decimal,
    /// Smallest permitted quantity increment. Order sizes must be a multiple of this.
    #[serde(with = "rust_decimal::serde::str")]
    pub lot_size: Decimal,
    /// Minimum order size accepted by the matching engine.
    #[serde(with = "rust_decimal::serde::str")]
    pub min_order_size: Decimal,
    /// Maximum order size accepted by the matching engine.
    #[serde(with = "rust_decimal::serde::str")]
    pub max_order_size: Decimal,
    /// Initial margin rate required to open a position (fraction of notional).
    #[serde(with = "rust_decimal::serde::str")]
    pub initial_margin_rate: Decimal,
    /// Maintenance margin rate below which a position is liquidated (fraction of notional).
    #[serde(with = "rust_decimal::serde::str")]
    pub maintenance_margin_rate: Decimal,
    /// Maximum leverage permitted on this market.
    pub max_leverage: u32,
}

/// Per-market summary with 24h volume and halt state.
///
/// `last_trade_price` and `volume_24h` arrive as JSON numbers via the `float`
/// adapter and may carry `f64` rounding artifacts — see the [module precision
/// note](crate::types#precision-of-float-adapter-fields).
#[derive(Debug, Clone, Deserialize)]
pub struct MarketSummary {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Last trade price as a JSON number — what the market last traded at, not
    /// the engine-derived mark price. `null` for a halted market with no recent
    /// trade (the spec types this `["number","null"]`).
    #[serde(with = "rust_decimal::serde::float_option")]
    pub last_trade_price: Option<Decimal>,
    /// Rolling 24-hour traded volume.
    #[serde(with = "rust_decimal::serde::float")]
    pub volume_24h: Decimal,
    /// Number of trades in the rolling 24-hour window.
    pub trade_count: u64,
    /// Market lifecycle state, e.g. `active`, `halted`.
    pub status: String,
    /// Reason the market was halted, if it is.
    pub halt_reason: Option<String>,
    /// Unix ms when the market was halted, if it is.
    pub halted_at: Option<i64>,
    /// Count of auto-deleveraging (ADL) events on this market.
    pub adl_event_count: u64,
}

/// Market lifecycle / halt status.
#[derive(Debug, Clone, Deserialize)]
pub struct MarketStatus {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Market lifecycle state, e.g. `active`, `halted`.
    pub status: String,
    /// Reason the market was halted, if it is.
    pub halt_reason: Option<String>,
    /// Unix ms when the market was halted, if it is.
    pub halted_at: Option<i64>,
    /// Count of auto-deleveraging (ADL) events on this market.
    pub adl_event_count: u64,
}

/// CCXT-style ticker. Price fields are optional — the API sends `null` when a
/// value is unavailable (e.g. no trades yet).
///
/// All [`Decimal`] fields here arrive as JSON numbers via the `float` adapter
/// and may carry `f64` rounding artifacts — see the [module precision
/// note](crate::types#precision-of-float-adapter-fields).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticker {
    /// Market symbol the ticker describes.
    pub symbol: String,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    /// ISO-8601 timestamp.
    pub datetime: String,
    /// Highest trade price in the period.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub high: Option<Decimal>,
    /// Lowest trade price in the period.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub low: Option<Decimal>,
    /// Best bid price.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub bid: Option<Decimal>,
    /// Size resting at the best bid.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub bid_volume: Option<Decimal>,
    /// Best ask price.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub ask: Option<Decimal>,
    /// Size resting at the best ask.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub ask_volume: Option<Decimal>,
    /// Opening price of the period.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub open: Option<Decimal>,
    /// Closing price of the period.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub close: Option<Decimal>,
    /// Most recent trade price.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub last: Option<Decimal>,
    /// Absolute price change over the period (`close - open`).
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub change: Option<Decimal>,
    /// Relative price change over the period, in percent.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub percentage: Option<Decimal>,
    /// Traded volume denominated in the base asset.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub base_volume: Option<Decimal>,
    /// Traded volume denominated in the quote asset.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub quote_volume: Option<Decimal>,
    /// Current mark price.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub mark_price: Option<Decimal>,
    /// Current index (oracle) price.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub index_price: Option<Decimal>,
    /// Raw exchange-specific payload.
    #[serde(default)]
    pub info: Value,
}

/// The caller's rate-limit status (`GET /account/rate-limit`).
///
/// Models a token bucket: `limit` is both the requests-per-second ceiling and
/// the burst capacity, `remaining` is the tokens available right now, and
/// `reset_at_ms` is when the bucket refills back to `limit` (`0` when full). All
/// three are `null` for the unlimited tier (gateway keys). Polling this endpoint
/// does not consume a token.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitStatus {
    /// Rate-limit tier name (e.g. `pro`, `marketmaker`, `unlimited`).
    pub tier: String,
    /// Maximum requests per second / burst capacity. `None` for the unlimited tier.
    pub limit: Option<u32>,
    /// Requests that can be made right now before throttling. `None` for the
    /// unlimited tier.
    pub remaining: Option<u32>,
    /// Unix timestamp (ms) when the bucket refills to `limit`; `0` when full.
    /// `None` for the unlimited tier.
    pub reset_at_ms: Option<i64>,
}

/// A single order-book level, `[price, amount]` (CCXT format).
///
/// Both values arrive as JSON numbers via the `float` adapter and may carry
/// `f64` rounding artifacts — see the [module precision
/// note](crate::types#precision-of-float-adapter-fields).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PriceLevel(
    /// Price at this level.
    #[serde(with = "rust_decimal::serde::float")]
    pub Decimal,
    /// Resting size at this level.
    #[serde(with = "rust_decimal::serde::float")]
    pub Decimal,
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
///
/// Level prices and sizes ([`PriceLevel`]) arrive as JSON numbers via the
/// `float` adapter and may carry `f64` rounding artifacts — see the [module
/// precision note](crate::types#precision-of-float-adapter-fields).
#[derive(Debug, Clone, Deserialize)]
pub struct OrderBook {
    /// Market symbol the book describes.
    pub symbol: String,
    /// Bid levels, highest price first.
    pub bids: Vec<PriceLevel>,
    /// Ask levels, lowest price first.
    pub asks: Vec<PriceLevel>,
    /// Unix timestamp (ms) of the snapshot.
    pub timestamp: i64,
    /// ISO-8601 timestamp of the snapshot.
    pub datetime: String,
    /// Monotonic sequence number for this snapshot.
    pub nonce: i64,
}

/// Order side. Serializes as PascalCase (`Buy`/`Sell`, as order endpoints
/// expect) and deserializes either case (public CCXT feeds use lowercase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Side {
    /// Buy / long side.
    #[serde(alias = "buy", alias = "BUY")]
    Buy,
    /// Sell / short side.
    #[serde(alias = "sell", alias = "SELL")]
    Sell,
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum OrderType {
    /// Rests on the book at a specified limit price.
    Limit,
    /// Executes immediately against resting liquidity at the best available price.
    Market,
    /// Triggerable stop that becomes a limit order once `trigger_price` is
    /// reached. Set [`OrderRequest::trigger_price`].
    StopLimit,
    /// Triggerable stop that becomes a market order once `trigger_price` is
    /// reached. Set [`OrderRequest::trigger_price`].
    StopMarket,
    /// Take-profit that becomes a limit order once `trigger_price` is reached.
    /// Set [`OrderRequest::trigger_price`].
    TakeProfitLimit,
    /// Take-profit that becomes a market order once `trigger_price` is reached.
    /// Set [`OrderRequest::trigger_price`].
    TakeProfitMarket,
    /// Trailing stop whose trigger tracks the market by a fixed offset. Set
    /// [`OrderRequest::trailing_offset_bps`].
    TrailingStop,
    /// Trailing stop that fires a *limit* order: the trigger tracks the market
    /// by [`trailing_offset_bps`](OrderRequest::trailing_offset_bps) and the
    /// fired limit price is offset from the trigger by
    /// [`limit_offset_bps`](OrderRequest::limit_offset_bps). Both offsets are
    /// required. Construct with [`OrderRequest::trailing_limit`].
    TrailingLimit,
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
    /// Post-only (add-liquidity-only): the order is rejected if it would take
    /// liquidity (cross the book) on entry, guaranteeing it rests as a maker.
    /// A crossing post-only order is rejected server-side with the
    /// `WouldTakeLiquidity` error (surfaced as [`TerminalError::InvalidOrder`]
    /// with that `code`).
    ///
    /// The wire value is `PostOnly` (PascalCase), so it opts out of the
    /// container's `UPPERCASE` renaming.
    ///
    /// [`TerminalError::InvalidOrder`]: crate::TerminalError::InvalidOrder
    #[serde(rename = "PostOnly")]
    PostOnly,
}

/// A public trade print.
///
/// `price`, `amount`, and `cost` arrive as JSON numbers via the `float` adapter
/// and may carry `f64` rounding artifacts — see the [module precision
/// note](crate::types#precision-of-float-adapter-fields). For the authoritative,
/// exact record of your own executions use [`Fill`], whose figures are
/// `str`-adapter exact.
#[derive(Debug, Clone, Deserialize)]
pub struct Trade {
    /// Exchange-assigned trade identifier.
    pub id: String,
    /// Market symbol the trade occurred on.
    pub symbol: String,
    /// Execution price.
    #[serde(with = "rust_decimal::serde::float")]
    pub price: Decimal,
    /// Executed size, in the base asset.
    #[serde(with = "rust_decimal::serde::float")]
    pub amount: Decimal,
    /// Notional value of the trade (`price * amount`), in the quote asset.
    #[serde(with = "rust_decimal::serde::float")]
    pub cost: Decimal,
    /// Aggressor side of the trade.
    pub side: Side,
    /// Unix timestamp (ms) of the trade.
    pub timestamp: i64,
    /// ISO-8601 timestamp of the trade.
    pub datetime: String,
    /// `taker` or `maker`, when known.
    #[serde(rename = "takerOrMaker")]
    pub taker_or_maker: Option<String>,
    /// Whether the trade resulted from a liquidation.
    pub is_liquidation: bool,
    /// Raw exchange-specific payload.
    #[serde(default)]
    pub info: Value,
}

/// An OHLCV candle, `[timestamp_ms, open, high, low, close, volume]` (CCXT format).
///
/// Every price/volume field arrives as a JSON number via the `float` adapter and
/// may carry `f64` rounding artifacts — see the [module precision
/// note](crate::types#precision-of-float-adapter-fields).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Ohlcv(
    /// Open time, Unix ms.
    pub i64,
    /// Open price.
    #[serde(with = "rust_decimal::serde::float")]
    pub Decimal,
    /// High price.
    #[serde(with = "rust_decimal::serde::float")]
    pub Decimal,
    /// Low price.
    #[serde(with = "rust_decimal::serde::float")]
    pub Decimal,
    /// Close price.
    #[serde(with = "rust_decimal::serde::float")]
    pub Decimal,
    /// Traded volume.
    #[serde(with = "rust_decimal::serde::float")]
    pub Decimal,
);

impl Ohlcv {
    /// Open time, Unix ms.
    pub fn timestamp(&self) -> i64 {
        self.0
    }
    /// Open price.
    pub fn open(&self) -> Decimal {
        self.1
    }
    /// High price.
    pub fn high(&self) -> Decimal {
        self.2
    }
    /// Low price.
    pub fn low(&self) -> Decimal {
        self.3
    }
    /// Close price.
    pub fn close(&self) -> Decimal {
        self.4
    }
    /// Traded volume.
    pub fn volume(&self) -> Decimal {
        self.5
    }
}

/// One intra-hour funding-rate sample.
#[derive(Debug, Clone, Deserialize)]
pub struct FundingSample {
    /// Unix timestamp (ms) of the sample.
    pub timestamp: i64,
    /// Funding rate at this sample (fraction of notional).
    #[serde(with = "rust_decimal::serde::str")]
    pub funding_rate: Decimal,
    /// Premium index (mark vs. oracle) at this sample.
    #[serde(with = "rust_decimal::serde::str")]
    pub premium_index: Decimal,
    /// Mark price at this sample.
    #[serde(with = "rust_decimal::serde::str")]
    pub mark_price: Decimal,
    /// Oracle (index) price at this sample.
    #[serde(with = "rust_decimal::serde::str")]
    pub oracle_price: Decimal,
}

/// Current mark price for a market.
#[derive(Debug, Clone, Deserialize)]
pub struct MarkPrice {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Current mark price.
    #[serde(with = "rust_decimal::serde::str")]
    pub mark_price: Decimal,
}

/// Aggregate service health (`GET /status`), the public health snapshot for the
/// indexer/engine/oracle/bots.
///
/// The v0.7.1 spec removed the old liveness `GET /health` / `GET /ready` probes;
/// `GET /status` (schema `ServiceHealth`) is the public replacement, so
/// [`Client::health_check`](crate::Client::health_check) now reads it. Rely on
/// the top-level [`status`](Self::status); [`services`](Self::services) carries
/// per-component detail that is informational and may evolve. Unknown fields are
/// ignored, so this stays forward-compatible as the snapshot grows.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthStatus {
    /// Worst-of health across all components: `ok`, `degraded`, `down`, or
    /// `starting`. Defaults to empty if the server omits it.
    #[serde(default)]
    pub status: String,
    /// Unix timestamp (ms) the snapshot was taken.
    #[serde(default)]
    pub timestamp_ms: i64,
    /// Per-component status (indexer, engine, oracle, bots), left untyped as the
    /// component detail is informational and may evolve. `Null` when absent.
    #[serde(default)]
    pub services: serde_json::Value,
}

/// An API key associated with the authenticated session (`GET /keys`).
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    /// Opaque identifier for the key.
    pub key_id: String,
    /// Rate-limit tier this key resolves to.
    pub tier: String,
}

/// A newly created API key (`POST /keys`).
///
/// The `secret` is returned **once** at creation and never again — persist it
/// immediately. The spec (v0.3.3) does not pin this response body; the field
/// names are inferred from the `GET /keys` shape, and `tier` defaults to absent
/// so an unexpectedly slim payload still decodes. The secret is held in a
/// [`SecretString`] (zeroized on drop, redacted in `Debug`); call
/// [`expose_secret`](secrecy::ExposeSecret::expose_secret) to read it once for
/// persistence.
#[derive(Deserialize)]
pub struct CreatedApiKey {
    /// Public key identifier (the `x-api-key` value for future requests).
    pub key_id: String,
    /// The HMAC secret, shown only on creation. Store it now; it is
    /// unrecoverable afterwards.
    #[serde(deserialize_with = "deserialize_secret")]
    pub secret: SecretString,
    /// Rate-limit tier the key resolves to, when the server reports it.
    #[serde(default)]
    pub tier: Option<String>,
}

/// Deserialize a JSON string into a [`SecretString`] so the value lands in
/// zeroizing storage rather than a plain heap `String`.
fn deserialize_secret<'de, D>(deserializer: D) -> Result<SecretString, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(SecretString::from(String::deserialize(deserializer)?))
}

impl fmt::Debug for CreatedApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedApiKey")
            .field("key_id", &self.key_id)
            // Never render the secret — it would otherwise leak into any log
            // line or panic message that formats this value.
            .field("secret", &"<redacted>")
            .field("tier", &self.tier)
            .finish()
    }
}

/// A registered agent key for the authenticated wallet (`GET /agents`).
///
/// The spec sends this object in camelCase and marks every field optional, so
/// the timestamps and label default rather than fail the decode when omitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    /// Agent address (0x-prefixed).
    pub address: String,
    /// Expiry, Unix ms.
    #[serde(default)]
    pub expires_at: i64,
    /// Registration time, Unix ms.
    #[serde(default)]
    pub registered_at: i64,
    /// Optional human-readable label.
    #[serde(default)]
    pub label: Option<String>,
}

/// Account balance and collateral summary (`GET /account`).
#[derive(Debug, Clone, Deserialize)]
pub struct AccountSummary {
    /// Cash balance.
    #[serde(with = "rust_decimal::serde::str")]
    pub balance: Decimal,
    /// Total collateral posted.
    #[serde(with = "rust_decimal::serde::str")]
    pub collateral: Decimal,
    /// Account equity (balance plus unrealized PnL).
    #[serde(with = "rust_decimal::serde::str")]
    pub equity: Decimal,
    /// Margin available to open new positions.
    #[serde(with = "rust_decimal::serde::str")]
    pub available_margin: Decimal,
    /// Currently open positions.
    pub positions: Vec<Position>,
}

/// An open position, with per-position risk detail.
///
/// # Enriched risk fields
///
/// The risk fields ([`leverage`](Self::leverage),
/// [`notional_value`](Self::notional_value), [`roe`](Self::roe),
/// [`margin_used`](Self::margin_used), [`max_leverage`](Self::max_leverage))
/// are derived server-side from indexer-mirrored state only — no engine
/// round-trip, to keep positions on the low-latency read path. When an input
/// isn't mirrored, the server sends the field as `null` and populates its
/// companion `*_error` with a machine-readable reason **instead of fabricating
/// a number**. So `None` never means "zero": pair each field with its `*_error`
/// to tell "not computable, because X" from a real value.
///
/// Every enriched field is `Option` and defaulted, so a position from a server
/// that predates them (or one that omits them entirely) still decodes rather
/// than failing the whole positions/balance/account-state read.
///
/// `#[non_exhaustive]`: the spec marks none of these properties required and is
/// openly planning more of them, so this is expected to keep gaining fields.
/// Match with a `..` rest pattern and read fields off a returned value rather
/// than constructing one with a struct literal.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct Position {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Position direction (e.g. `long`/`short`).
    pub side: String,
    /// Position size, in the base asset.
    #[serde(with = "rust_decimal::serde::str")]
    pub size: Decimal,
    /// Average entry price.
    #[serde(with = "rust_decimal::serde::str")]
    pub entry_price: Decimal,
    /// Unrealized profit and loss at the current mark price.
    #[serde(with = "rust_decimal::serde::str")]
    pub unrealized_pnl: Decimal,
    /// Realized profit and loss booked so far.
    #[serde(with = "rust_decimal::serde::str")]
    pub realized_pnl: Decimal,
    /// Liquidation price. The spec does not mark it required (it can be absent
    /// in flat / cross-margin states), so it's optional rather than hard-failing
    /// the whole balance/positions decode when omitted.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub liquidation_price: Option<Decimal>,
    /// Position leverage (the account's leverage multiplier for this position).
    ///
    /// Currently always `None` — deriving it needs the account's leverage
    /// setting or equity/allocated margin, which the indexer does not mirror;
    /// [`leverage_error`](Self::leverage_error) carries the reason. Do **not**
    /// infer leverage from [`margin_used`](Self::margin_used): that collapses to
    /// `1 / initial_margin_rate`, a per-market constant, not the real leverage.
    ///
    /// Sent as a JSON *number*, so this uses the `float` serde adapter and is
    /// subject to the precision caveat in the [module docs](self).
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub leverage: Option<Decimal>,
    /// Why [`leverage`](Self::leverage) is `None` (currently always
    /// `margin_state_not_mirrored`), or `None` when it is populated.
    #[serde(default)]
    pub leverage_error: Option<String>,
    /// Position notional value (`|size| × mark price`). `None` when the mark
    /// price is unavailable — see
    /// [`notional_value_error`](Self::notional_value_error).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub notional_value: Option<Decimal>,
    /// Why [`notional_value`](Self::notional_value) is `None` (e.g.
    /// `mark_price_unavailable`), or `None` when it is populated.
    #[serde(default)]
    pub notional_value_error: Option<String>,
    /// Return on equity: `unrealized_pnl / margin_used` (return on initial
    /// margin). `None` when an input is unavailable or margin is zero — see
    /// [`roe_error`](Self::roe_error).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub roe: Option<Decimal>,
    /// Why [`roe`](Self::roe) is `None` (e.g. `mark_price_unavailable`,
    /// `margin_rate_unavailable`, `margin_used_zero`), or `None` when it is
    /// populated.
    #[serde(default)]
    pub roe_error: Option<String>,
    /// Initial-margin requirement held against this position
    /// (`notional_value × initial_margin_rate`, under the engine's cross-margin
    /// model). Isolated/custom margin allocations are not mirrored by the
    /// indexer. `None` when an input is unavailable — see
    /// [`margin_used_error`](Self::margin_used_error).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub margin_used: Option<Decimal>,
    /// Why [`margin_used`](Self::margin_used) is `None` (e.g.
    /// `mark_price_unavailable`, `margin_rate_unavailable`), or `None` when it
    /// is populated.
    #[serde(default)]
    pub margin_used_error: Option<String>,
    /// Maximum leverage allowed for this market, from the market's risk
    /// parameters. `None` when those params are unavailable — see
    /// [`max_leverage_error`](Self::max_leverage_error).
    #[serde(default)]
    pub max_leverage: Option<u32>,
    /// Why [`max_leverage`](Self::max_leverage) is `None` (e.g.
    /// `market_params_unavailable`), or `None` when it is populated.
    #[serde(default)]
    pub max_leverage_error: Option<String>,
    /// Cumulative funding paid on this position.
    ///
    /// **Paid-positive**: a positive value means the position has *paid*
    /// funding, a negative value means it has *received* funding. The server
    /// always sends it (`"0"` when nothing has accrued), bounded by the funding
    /// history the indexer retains; it is `Option` only so a position from a
    /// server that predates the field still decodes.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub funding_paid: Option<Decimal>,
}

/// A closed position (`GET /api/v1/positions/closed`, spec v0.7.2).
///
/// The realized counterpart of [`Position`]: the size and prices at close plus
/// the PnL the close booked. [`side`](Self::side) is the side the position held
/// **before** it closed — note the wire form here is `Long` / `Short`, not the
/// `buy` / `sell` of orders and fills — and [`size`](Self::size) is its absolute
/// size at close, so the direction lives in `side` alone.
///
/// # Every field is optional
///
/// The spec gives this schema **no `required` array**, so every field is `Option`
/// and defaulted, exactly as on [`AccountPortfolioSummary`]. `None` means *not
/// reported* — never zero. That distinction is the whole point on
/// [`realized_pnl`](Self::realized_pnl) and [`exit_price`](Self::exit_price):
/// defaulting an absent field to `Decimal::ZERO` would report a loss-making close
/// as break-even and an unknown exit price as free, with nothing to tell the
/// caller it was fabricated. (The Python SDK keeps the untouched payload on
/// `raw` for the same reason; this SDK has no `raw`, so absence is modelled in
/// the type instead.)
///
/// `#[non_exhaustive]`: read fields off a returned value rather than constructing
/// one with a struct literal, so a future spec addition isn't a breaking change.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ClosedPosition {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    #[serde(default)]
    pub market_id: Option<String>,
    /// The side the position held before it closed: `Long` or `Short`.
    ///
    /// An open string rather than an enum so a side spelling added to a later
    /// spec still decodes instead of failing the whole page.
    #[serde(default)]
    pub side: Option<String>,
    /// Absolute position size at close, in the base asset (unsigned — the
    /// direction is [`side`](Self::side)).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub size: Option<Decimal>,
    /// Average entry price of the closed position.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub entry_price: Option<Decimal>,
    /// Price the position closed at.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub exit_price: Option<Decimal>,
    /// Profit and loss the close realized. Signed: negative is a loss.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub realized_pnl: Option<Decimal>,
    /// Unix timestamp (ms) the position closed at. `None` when unreported —
    /// **not** `0`, which would date every such close to the Unix epoch.
    #[serde(default)]
    pub closed_at_ms: Option<i64>,
}

/// Portfolio summary for the authenticated account
/// (`GET /api/v1/account/summary`) — aggregate equity, PnL, volume, and open
/// counts.
///
/// Distinct from [`AccountSummary`], which is the balance/collateral view from
/// `GET /api/v1/account` and embeds the account's positions.
///
/// # Every field is optional
///
/// The spec gives this schema **no `required` array**, so the server may
/// legitimately omit any property. Each field is therefore `Option` and
/// defaulted: an absent `collateral` yields `None` for that one field instead of
/// failing the entire `/account/summary` or `/account/state` decode. `None` means
/// "not reported", never zero — do not substitute `0` for a missing aggregate, or
/// an underwater account reads as flat. In practice a current server sends all of
/// them.
///
/// `#[non_exhaustive]`: read fields off a returned value rather than constructing
/// one with a struct literal, so a future spec addition isn't a breaking change.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AccountPortfolioSummary {
    /// Collateral posted to the account.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub collateral: Option<Decimal>,
    /// Total account equity (collateral plus unrealized PnL).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub total_equity: Option<Decimal>,
    /// Total unrealized PnL across all open positions.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub total_unrealized_pnl: Option<Decimal>,
    /// Realized PnL booked over the last 24 hours.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub total_realized_pnl_24h: Option<Decimal>,
    /// Traded notional over the last 24 hours.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub total_volume_24h: Option<Decimal>,
    /// Number of open positions. When present on an [`AccountState`] read this
    /// equals `positions.len()`.
    #[serde(default)]
    pub open_positions_count: Option<u32>,
    /// Number of resting open orders.
    #[serde(default)]
    pub open_orders_count: Option<u32>,
    /// Margin currently held against open positions.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub margin_used: Option<Decimal>,
    /// Margin available to open new positions. May be negative for an
    /// underwater account; see [`withdrawable`](Self::withdrawable) for the
    /// floored, actually-withdrawable figure.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub available_margin: Option<Decimal>,
    /// Wallet-withdrawable balance: engine-authoritative free margin floored at
    /// zero (`max(0, available_margin)`).
    ///
    /// Free margin already nets each position's initial margin and pre-trade
    /// order reservations out of equity, so this is exactly what can leave the
    /// account. A negative free margin (an underwater account) is clamped to
    /// `0` and never surfaced negative. The server derives it from the
    /// authoritative margin view and fails closed with `502` rather than
    /// reporting a local estimate when that view is unavailable — so a value
    /// here is authoritative, never an approximation. That 502 surfaces as
    /// [`TransientError::Unavailable`](crate::TransientError::Unavailable) with
    /// [`code`](crate::Error::code) `authoritative_margin_unavailable`: retry the
    /// read, and do not read the failure as a zero balance.
    ///
    /// `None` when the server did not report it (e.g. one predating the field);
    /// prefer it over [`available_margin`](Self::available_margin) when deciding
    /// how much a user may withdraw.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub withdrawable: Option<Decimal>,
    /// Whether the account is allowed through the early-access gate. `None`
    /// unless that gate is active.
    #[serde(default)]
    pub early_access_allowed: Option<bool>,
}

/// Consolidated single-call account snapshot (`GET /api/v1/account/state`) — the
/// portfolio summary plus every open position.
///
/// Both halves come from **one coherent server-side read**, so they cannot tear
/// against each other: `summary.open_positions_count` (when reported) always
/// equals `positions.len()`, and `summary` is the same value the standalone
/// [`Client::fetch_account_summary`](crate::Client::fetch_account_summary)
/// returns. Fetching this is therefore strictly safer than issuing
/// `fetch_account_summary` and
/// [`fetch_positions`](crate::Client::fetch_positions) concurrently, where a
/// fill landing between the two responses yields a mismatched pair.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AccountState {
    /// Aggregate portfolio summary for the account.
    pub summary: AccountPortfolioSummary,
    /// All open positions for the account.
    pub positions: Vec<Position>,
}

/// Window selector for the portfolio time series
/// ([`Client::fetch_portfolio_history`](crate::Client::fetch_portfolio_history)).
///
/// The window also fixes the server-side downsample cadence and point capacity:
///
/// | window | cadence | max points | span |
/// |---|---|---|---|
/// | [`Day`](Self::Day) | 5 min | 288 | 24 h |
/// | [`Week`](Self::Week) | 1 h | 168 | 7 d |
/// | [`Month`](Self::Month) | 6 h | 120 | 30 d |
/// | [`All`](Self::All) | 1 d | 366 | ~1 y |
///
/// Serializes lowercase (`day` / `week` / `month` / `all`), as the `window`
/// query parameter expects, and deserializes those same four spellings — the
/// spec's enum is lowercase-only, so no other casing is recognized.
///
/// This is the **request** side. A served window is read back off
/// [`PortfolioHistory::window`], which is the raw string so that a window added
/// to a later spec still decodes; [`from_wire`](Self::from_wire) maps it onto
/// this enum where it can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortfolioWindow {
    /// Trailing 24 hours, sampled every 5 minutes. The server's default.
    #[default]
    Day,
    /// Trailing 7 days, sampled hourly.
    Week,
    /// Trailing 30 days, sampled every 6 hours.
    Month,
    /// Full retained history (~1 year), sampled daily.
    All,
}

impl PortfolioWindow {
    /// The wire value for this window, as sent in the `window` query parameter.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::All => "all",
        }
    }

    /// Map a served wire value onto this enum, the inverse of
    /// [`as_str`](Self::as_str).
    ///
    /// `None` for anything outside the spec's four members — most usefully, a
    /// window added to a later spec. Callers reading
    /// [`PortfolioHistory::window`] get the served string either way, so an
    /// unrecognized window stays reportable instead of being lost; the
    /// `spec-drift` gate fails loudly the moment the spec adds a member, which is
    /// the signal to add the variant here.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

impl fmt::Display for PortfolioWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One downsampled sample from the portfolio time series.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct PortfolioPoint {
    /// Sample time, Unix ms.
    pub timestamp_ms: i64,
    /// Account equity at sample time (collateral plus unrealized PnL).
    #[serde(with = "rust_decimal::serde::str")]
    pub equity: Decimal,
    /// Cumulative trading PnL up to this sample: realized PnL on close
    /// (including liquidation and ADL closes), plus signed funding, plus current
    /// unrealized PnL.
    ///
    /// Deposit-neutral — wallet deposits and withdrawals never move it — so the
    /// curve reflects trading performance only.
    #[serde(with = "rust_decimal::serde::str")]
    pub pnl: Decimal,
    /// Cumulative traded notional (`Σ price × size`) up to this sample, across
    /// taker and maker fills, counting a self-trade once. Monotonically
    /// non-decreasing.
    #[serde(with = "rust_decimal::serde::str")]
    pub volume: Decimal,
}

/// Portfolio time series for the authenticated account
/// (`GET /api/v1/account/portfolio-history`): equity, cumulative PnL, and
/// cumulative volume over the requested window.
///
/// The spec marks all three properties `required`, and all three decode
/// strictly: an absent or `null` `window`, `cadence_ms` or `points` fails the
/// decode loudly rather than being defaulted. A missing `cadence_ms` must not
/// read as `0` (caller arithmetic divides by it), and a dropped `points` array
/// must not read as "no history" — silently substituting an empty series would
/// present a flat chart the server never reported. This matches
/// [`AccountFees`]'s policy, and the Python SDK's.
///
/// The one concession to forward compatibility is that
/// [`window`](Self::window) is an open string rather than the
/// [`PortfolioWindow`] enum — see that field.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct PortfolioHistory {
    /// The window actually served — echoes the requested
    /// [`PortfolioWindow`], or the server's `day` default when none was sent.
    /// Read this rather than assuming the request's value.
    ///
    /// An **open string**, not the enum, so a window added to a later spec still
    /// decodes: failing an entire response over a *label* would discard the
    /// [`points`](Self::points) that are the actual payload, and keeping the
    /// served text means an unrecognized window is still reportable — a caller
    /// can log or display `"quarter"` rather than lose it. Use
    /// [`PortfolioWindow::from_wire`] (or
    /// [`window_parsed`](Self::window_parsed)) for the typed form, and
    /// [`cadence_ms`](Self::cadence_ms) for the sample interval regardless.
    ///
    /// Absent or `null` is a contract violation and fails the decode; it is not
    /// reported as an empty string.
    pub window: String,
    /// Downsample interval between adjacent points, in milliseconds (e.g.
    /// `300000` for [`PortfolioWindow::Day`]).
    pub cadence_ms: i64,
    /// Samples for the window, **oldest first**. Bounded by the window's point
    /// capacity and by the request's `limit`. Empty for an account with no
    /// history in the window — but an *absent* array fails the decode rather
    /// than reading as empty.
    pub points: Vec<PortfolioPoint>,
}

impl PortfolioHistory {
    /// [`window`](Self::window) as a [`PortfolioWindow`], or `None` if the server
    /// served one this SDK version cannot name.
    ///
    /// Convenience for [`PortfolioWindow::from_wire`]; the raw value stays
    /// readable on [`window`](Self::window) either way.
    pub fn window_parsed(&self) -> Option<PortfolioWindow> {
        PortfolioWindow::from_wire(&self.window)
    }
}

/// One equity sample (`GET /api/v1/account/equity-history`, spec v0.7.2).
///
/// The high-resolution recent view of account equity — 5s cadence over roughly a
/// one-hour window, **oldest first** — where [`PortfolioHistory`] is the
/// downsampled long-window one. The two are derived from the same underlying
/// value, so compare them by decimal value rather than by wire text.
///
/// # `equity` arrives as a JSON *number*, not a decimal string
///
/// This is the one place the two series disagree on the wire:
/// [`PortfolioPoint::equity`] is a lossless decimal string, while `equity` here
/// is a JSON number, so it decodes through the `float` adapter and carries the
/// precision caveat in the [module docs](self). Read anything authoritative for
/// accounting off a `str`-adapter field ([`PortfolioPoint::equity`],
/// [`AccountPortfolioSummary::total_equity`]) and treat this series as what it is:
/// a fine-grained chart source.
///
/// # Every field is optional
///
/// The spec gives this schema **no `required` array**, so both fields are `Option`
/// and defaulted, as on [`AccountPortfolioSummary`]. `None` means *not reported*,
/// never zero: an equity sample defaulted to `0` would draw a wiped-out account
/// the server never reported, and a timestamp defaulted to `0` would place the
/// sample at the Unix epoch.
///
/// `#[non_exhaustive]`: read fields off a returned value rather than constructing
/// one with a struct literal, so a future spec addition isn't a breaking change.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct EquityPoint {
    /// Sample time, Unix ms. `None` when unreported — **not** `0`.
    #[serde(default)]
    pub timestamp_ms: Option<i64>,
    /// Account equity at sample time (collateral balance plus Σ unrealized PnL).
    ///
    /// Sent as a JSON *number*, so this uses the `float` serde adapter and is
    /// subject to the precision caveat in the [module docs](self) — see the type
    /// docs above.
    #[serde(default, with = "rust_decimal::serde::float_option")]
    pub equity: Option<Decimal>,
}

/// The authenticated account's effective fee schedule
/// (`GET /api/v1/account/fees`).
///
/// Reports the **forward-looking schedule rate**, not a realized per-fill
/// average.
///
/// Unlike [`AccountPortfolioSummary`], the spec marks every field here required,
/// so the rates and volume are non-`Option`: an omission is a contract violation
/// and fails the decode loudly rather than being silently defaulted. Defaulting a
/// fee to `0` bps would read as "trading is free", and a defaulted `volume_30d`
/// as "no volume" — figures the server never reported.
///
/// [`discounts`](Self::discounts) is the single documented exception, for the
/// reason given on that field. Every other field decodes strictly.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AccountFees {
    /// Effective maker fee, in basis points. **Negative means the maker is paid
    /// a rebate** — e.g. `-2` is a 0.02% rebate — so this is deliberately
    /// signed.
    pub maker_fee_bps: i32,
    /// Effective taker fee, in basis points — e.g. `5` is a 0.05% fee.
    pub taker_fee_bps: i32,
    /// Fee tier for the account. Currently always `base`: there are no
    /// per-account fee tiers yet (distinct from rate-limit tiers). Treat as an
    /// **open string** — new values appear when the fee model lands.
    pub tier: String,
    /// Scope of the reported rate, currently always `standard`.
    ///
    /// The venue charges a per-market schedule (standard crypto, mid-cap crypto,
    /// FX, and commodities/indices all differ), but this endpoint takes no
    /// market parameter, so it reports the standard crypto-group schedule and
    /// marks it here. Treat the rate as scoped by this value, **not** a
    /// venue-wide guarantee. Also an open string.
    pub schedule: String,
    /// Rolling 30-day traded notional for the account. Best-effort — see
    /// [`volume_30d_estimated`](Self::volume_30d_estimated).
    #[serde(with = "rust_decimal::serde::str")]
    pub volume_30d: Decimal,
    /// `true` when [`volume_30d`](Self::volume_30d) may **undercount**: the
    /// source fill buffer was at capacity, so some older in-window fills may
    /// have been evicted. `false` when the full 30-day window is covered.
    pub volume_30d_estimated: bool,
    /// Active fee discounts on the account. Currently always empty — no
    /// discount program exists yet.
    ///
    /// The **one exception** to this type's strict decode: the spec requires this
    /// key, but an absent one defaults to `[]` rather than failing the read. The
    /// justification is specific to this field and does not generalize — unlike a
    /// time series or a position list, a dropped discount cannot silently distort
    /// a figure the caller computes, so tolerating the omission cannot make any
    /// number wrong. Every strict field on this type is one whose default *would*
    /// misreport something.
    ///
    /// Matches the Python SDK, deliberately: the two SDKs report an absent
    /// `discounts` the same way, so a caller need not know which language they
    /// are in to know whether it raises. A **malformed** entry (a non-object in
    /// the array) still fails the decode, which is where the two differ — py
    /// skips it.
    #[serde(default)]
    pub discounts: Vec<FeeDiscount>,
}

/// An active fee discount applied to the account.
///
/// The concrete shape is **provisional** and finalizes with the fee model, so
/// the spec guarantees no properties yet and
/// [`AccountFees::discounts`] is currently always empty. Rather than freeze a
/// shape that is about to change, this preserves the server's object verbatim —
/// read [`fields`](Self::fields) directly, and expect a typed replacement once
/// the fee model lands.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct FeeDiscount {
    /// The raw discount object as sent by the server.
    pub fields: serde_json::Map<String, Value>,
}

/// A fill (private trade execution) for the authenticated account.
#[derive(Debug, Clone, Deserialize)]
pub struct Fill {
    /// Exchange-assigned fill identifier.
    pub id: String,
    /// Identifier of the order this fill belongs to.
    pub order_id: String,
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Side of the filled order.
    pub side: Side,
    /// Execution price.
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    /// Executed size, in the base asset.
    #[serde(with = "rust_decimal::serde::str")]
    pub size: Decimal,
    /// Fee charged for this fill (negative for a rebate).
    #[serde(with = "rust_decimal::serde::str")]
    pub fee: Decimal,
    /// `taker` or `maker`, when reported.
    #[serde(default)]
    pub taker_or_maker: Option<String>,
    /// Unix timestamp (ms) of the fill.
    pub timestamp: i64,
    /// Whether the fill resulted from a liquidation.
    pub is_liquidation: bool,
}

/// A new-order request (`POST /orders`). Construct with [`OrderRequest::limit`]
/// or [`OrderRequest::market`].
#[derive(Debug, Clone, Serialize)]
pub struct OrderRequest {
    /// Market identifier to trade, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Order side.
    pub side: Side,
    /// Order type.
    pub order_type: OrderType,
    /// Limit price; omitted for market orders.
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "rust_decimal::serde::str_option"
    )]
    pub price: Option<Decimal>,
    /// Order size, in the base asset.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Time-in-force policy.
    pub time_in_force: TimeInForce,
    /// When set, the order may only reduce an existing position, never open or
    /// flip one. Omitted from the wire payload when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduce_only: Option<bool>,
    /// Caller-assigned client order id, echoed back on the resulting order and
    /// usable to look it up or cancel it via
    /// [`Client::fetch_order_by_client_id`](crate::Client::fetch_order_by_client_id)
    /// / [`Client::cancel_order_by_client_id`](crate::Client::cancel_order_by_client_id).
    /// Omitted from the wire payload when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    /// Trigger threshold for the triggerable, non-trailing order types
    /// ([`StopLimit`](OrderType::StopLimit), [`StopMarket`](OrderType::StopMarket),
    /// [`TakeProfitLimit`](OrderType::TakeProfitLimit),
    /// [`TakeProfitMarket`](OrderType::TakeProfitMarket)). Ignored for the other
    /// types. Omitted from the wire payload when `None`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "rust_decimal::serde::str_option"
    )]
    pub trigger_price: Option<Decimal>,
    /// Trailing offset in basis points (1 bp = 0.01%). Required for
    /// [`TrailingStop`](OrderType::TrailingStop) and
    /// [`TrailingLimit`](OrderType::TrailingLimit); ignored otherwise. Omitted
    /// from the wire payload when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailing_offset_bps: Option<u32>,
    /// Offset in basis points for the fired limit price, for
    /// [`TrailingLimit`](OrderType::TrailingLimit) only (required together with
    /// [`trailing_offset_bps`](Self::trailing_offset_bps)). Omitted from the wire
    /// payload when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_offset_bps: Option<u32>,
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
            client_order_id: None,
            trigger_price: None,
            trailing_offset_bps: None,
            limit_offset_bps: None,
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
            client_order_id: None,
            trigger_price: None,
            trailing_offset_bps: None,
            limit_offset_bps: None,
        }
    }

    /// A [`TrailingLimit`](OrderType::TrailingLimit) order: the trigger trails
    /// the market by `trailing_offset_bps` and, once fired, rests as a limit
    /// order offset from the trigger by `limit_offset_bps` (both in basis
    /// points, 1 bp = 0.01%).
    pub fn trailing_limit(
        market_id: impl Into<String>,
        side: Side,
        quantity: Decimal,
        trailing_offset_bps: u32,
        limit_offset_bps: u32,
        time_in_force: TimeInForce,
    ) -> Self {
        Self {
            market_id: market_id.into(),
            side,
            order_type: OrderType::TrailingLimit,
            price: None,
            quantity,
            time_in_force,
            reduce_only: None,
            client_order_id: None,
            trigger_price: None,
            trailing_offset_bps: Some(trailing_offset_bps),
            limit_offset_bps: Some(limit_offset_bps),
        }
    }

    /// Attach a caller-assigned client order id, consuming and returning `self`
    /// so it chains off [`limit`](Self::limit) / [`market`](Self::market).
    pub fn with_client_order_id(mut self, client_order_id: impl Into<String>) -> Self {
        self.client_order_id = Some(client_order_id.into());
        self
    }

    /// Set the trigger threshold for a triggerable, non-trailing order (stop /
    /// take-profit), consuming and returning `self`.
    pub fn with_trigger_price(mut self, trigger_price: Decimal) -> Self {
        self.trigger_price = Some(trigger_price);
        self
    }

    /// Set the trailing offset (basis points) for a
    /// [`TrailingStop`](OrderType::TrailingStop) /
    /// [`TrailingLimit`](OrderType::TrailingLimit) order, consuming and returning
    /// `self`.
    pub fn with_trailing_offset_bps(mut self, trailing_offset_bps: u32) -> Self {
        self.trailing_offset_bps = Some(trailing_offset_bps);
        self
    }

    /// Set the fired-limit-price offset (basis points) for a
    /// [`TrailingLimit`](OrderType::TrailingLimit) order, consuming and returning
    /// `self`.
    pub fn with_limit_offset_bps(mut self, limit_offset_bps: u32) -> Self {
        self.limit_offset_bps = Some(limit_offset_bps);
        self
    }
}

/// An order record.
#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    /// Exchange-assigned order identifier.
    pub id: String,
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    // The spec marks every Order field optional, so the non-identity, non-enum
    // fields default rather than fail deserialization if the API omits them.
    /// Identifier of the account that owns the order.
    #[serde(default)]
    pub account_id: String,
    /// Order side.
    pub side: Side,
    /// Order type.
    pub order_type: OrderType,
    /// Limit price; `None` for market orders.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub price: Option<Decimal>,
    /// Original order size, in the base asset.
    #[serde(default, with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Quantity filled so far, in the base asset.
    #[serde(default, with = "rust_decimal::serde::str")]
    pub filled_qty: Decimal,
    /// `Open`, `PartiallyFilled`, `Filled`, `Cancelled`, `Expired`, `Rejected`.
    #[serde(default)]
    pub status: String,
    /// Time-in-force policy.
    pub time_in_force: TimeInForce,
    /// Caller-assigned client order id, if one was supplied when the order was
    /// placed. The spec marks it optional, so it defaults to `None` when absent.
    #[serde(default)]
    pub client_order_id: Option<String>,
    /// Fired-limit-price offset in basis points for a
    /// [`TrailingLimit`](OrderType::TrailingLimit) order (mirrors
    /// [`OrderRequest::limit_offset_bps`]). `None` for order types that don't
    /// carry one, or when the API omits it.
    #[serde(default)]
    pub limit_offset_bps: Option<u32>,
    /// Unix timestamp (ms) when the order was created.
    #[serde(default)]
    pub created_at: i64,
    /// Unix timestamp (ms) when the order was last updated.
    #[serde(default)]
    pub updated_at: i64,
}

/// A terminal-status order (`GET /api/v1/orders/history`, spec v0.7.2).
///
/// Orders that have reached `Filled` / `Cancelled` / `Rejected` / `Expired`,
/// newest first. Distinct from [`Order`], which
/// [`fetch_open_orders`](crate::Client::fetch_open_orders) returns for *live*
/// orders: the history entry drops the live bookkeeping fields (`account_id`,
/// `time_in_force`, `updated_at`) and adds
/// [`completed_at_ms`](Self::completed_at_ms) and
/// [`cancellation_reason`](Self::cancellation_reason).
///
/// [`size`](Self::size) is the **original** quantity, not the remaining one —
/// compare it against [`filled_qty`](Self::filled_qty) to see how much of a
/// cancelled order had executed before it went away.
///
/// # Every field is optional
///
/// The spec gives this schema **no `required` array**, so every field is `Option`
/// and defaulted, as on [`AccountPortfolioSummary`]. `None` means *not reported*,
/// and it is deliberately distinguishable from the zero/empty value in each case:
/// a defaulted `""` [`status`](Self::status) would read as a status the server
/// never sent, a defaulted `0` [`filled_qty`](Self::filled_qty) would report a
/// partially-filled cancel as untouched, and a defaulted `0` timestamp would date
/// the order to the Unix epoch. (The Python SDK keeps the untouched payload on
/// `raw` to recover the same distinction; this SDK has no `raw`, so absence is
/// modelled in the type.)
///
/// `#[non_exhaustive]`: read fields off a returned value rather than constructing
/// one with a struct literal, so a future spec addition isn't a breaking change.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct OrderHistoryEntry {
    /// Exchange-assigned order identifier.
    #[serde(default)]
    pub id: Option<String>,
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    #[serde(default)]
    pub market_id: Option<String>,
    /// Order side.
    #[serde(default)]
    pub side: Option<Side>,
    /// Order type: `limit`, `market`, `stop_*`, `take_profit_*`,
    /// `trailing_stop`, `trailing_limit`.
    ///
    /// An open string rather than [`OrderType`]: the spec documents this
    /// property as free-form prose over a lowercase, snake-cased vocabulary
    /// (`stop_limit`), not the `PascalCase` enum [`OrderType`] serializes, and
    /// keeping it open means an order type added upstream still decodes.
    #[serde(default)]
    pub order_type: Option<String>,
    /// Limit price. The spec types this **nullable** — a market order carries no
    /// limit price — so an explicit `null` decodes to `None` rather than a
    /// fabricated `0` that would read as a real price of zero.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub price: Option<Decimal>,
    /// **Original** order quantity, in the base asset (not the remaining one).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub size: Option<Decimal>,
    /// Quantity filled before the order reached its terminal status, in the base
    /// asset. `None` is *not reported*, distinct from a real `0` (nothing filled).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub filled_qty: Option<Decimal>,
    /// Terminal status: `Filled`, `Cancelled`, `Rejected`, or `Expired`.
    ///
    /// An open string rather than an enum so a status added to a later spec still
    /// decodes, matching [`Order::status`].
    #[serde(default)]
    pub status: Option<String>,
    /// Why the order was cancelled, when the server reports a reason. The spec
    /// types it nullable, so `None` covers both "no reason given" and "not
    /// reported".
    #[serde(default)]
    pub cancellation_reason: Option<String>,
    /// Unix timestamp (ms) the order was created. `None` when unreported —
    /// **not** `0`.
    #[serde(default)]
    pub created_at_ms: Option<i64>,
    /// Unix timestamp (ms) the order reached its terminal status. `None` when
    /// unreported — **not** `0`.
    #[serde(default)]
    pub completed_at_ms: Option<i64>,
}

/// Response to `POST /orders`: the resulting order plus any immediate fills.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    /// The created or updated order.
    pub order: Order,
    /// Immediate fills (currently untyped in the spec).
    #[serde(default)]
    pub fills: Vec<serde_json::Value>,
}

/// Projected pre-trade impact of an order that was **not** submitted
/// (`POST /api/v1/orders/preview`, spec schema `PreviewResponse`) — returned by
/// [`Client::preview_order`](crate::Client::preview_order).
///
/// # A rejected preview is a success, not an error
///
/// The endpoint answers "what would this order do?", so a *projection saying the
/// order would be rejected* is a `200` carrying
/// [`accepted`](Self::accepted)` = Some(false)` and a
/// [`reject_reason`](Self::reject_reason) — **not** an `Err`. Only a genuine
/// request failure (bad request, auth, rate limit, server) is an `Err`. Always
/// branch on [`is_accepted`](Self::is_accepted) rather than on `Result::is_ok`,
/// or a would-be-rejected order reads as safe to send.
///
/// # Every field is optional
///
/// The spec gives this schema **no `required` array**, so the server may
/// legitimately omit any property. Each field is therefore `Option` and
/// defaulted: an absent `projected_fees` yields `None` for that one field instead
/// of failing the whole preview decode. `None` means "not reported", never zero —
/// do not substitute `0` for a missing projection, or an order that needs margin
/// reads as free. In practice a current server sends all of them.
///
/// Every monetary field is a decimal **string** on the wire, parsed exactly via
/// the `str` adapter — including
/// [`projected_post_trade_leverage`](Self::projected_post_trade_leverage), which
/// the spec types as `Decimal` (a string) even though the *request*-side
/// `leverage` parameter elsewhere in the API is a JSON number.
///
/// `#[non_exhaustive]`: read fields off a returned value rather than constructing
/// one with a struct literal, so a future spec addition isn't a breaking change.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct OrderPreview {
    /// Whether the order would be accepted if submitted as-is. `None` when the
    /// server did not report it — treat that as "unknown", never as accepted;
    /// [`is_accepted`](Self::is_accepted) does exactly that.
    #[serde(default)]
    pub accepted: Option<bool>,
    /// Why the order would be rejected, when
    /// [`accepted`](Self::accepted) is `Some(false)`; `None` for an accepted
    /// preview (the server sends `null`).
    ///
    /// Deliberately a free-form `String` rather than an enum: the spec types it
    /// as a plain nullable string with no enumerated values, so a reason added
    /// server-side must not fail the decode. Match on it for display/telemetry,
    /// not for control flow.
    #[serde(default)]
    pub reject_reason: Option<String>,
    /// Initial margin the order would require.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub required_initial_margin: Option<Decimal>,
    /// Account equity projected after the order fills.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub projected_post_trade_equity: Option<Decimal>,
    /// Liquidation price projected after the order fills. `None` (wire `null`)
    /// when the resulting state has no liquidation price — e.g. the order would
    /// flatten the position.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub projected_post_trade_liquidation_price: Option<Decimal>,
    /// Account leverage projected after the order fills. A decimal string on the
    /// wire, not a JSON number.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub projected_post_trade_leverage: Option<Decimal>,
    /// Volume-weighted average price the order is expected to fill at. `None`
    /// (wire `null`) when no fill is projected — e.g. a resting limit order that
    /// would not cross.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub expected_fill_vwap: Option<Decimal>,
    /// Fees the order is expected to incur.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub projected_fees: Option<Decimal>,
}

impl OrderPreview {
    /// Whether the order would be accepted, **failing closed**: an unreported
    /// [`accepted`](Self::accepted) returns `false`.
    ///
    /// Use this to gate submission. Reading the raw `Option` and treating `None`
    /// as "fine" would send an order the server never vouched for.
    pub fn is_accepted(&self) -> bool {
        self.accepted == Some(true)
    }
}

/// One entry in the array returned by
/// [`Client::create_orders`](crate::Client::create_orders) (`POST
/// /orders/batch`).
///
/// The batch is processed sequentially and non-atomically, so the array
/// preserves request order and each entry independently reports either a placed
/// order or a per-order rejection. The HTTP status is `201` for the batch as a
/// whole even when individual entries failed — an early order consuming margin
/// can reject a later one without aborting the batch — so per-order outcomes
/// live *inside* each entry rather than in the response status.
///
/// On the wire each entry is internally tagged by an `outcome` field
/// (`"ok"`/`"err"`): a placed entry carries the same `{ order, fills }` shape as
/// the single-order [`OrderResponse`], and a rejected entry carries the same
/// `{ error, message }` shape as the global error envelope. Match on the variant
/// (or use [`succeeded`](Self::succeeded) / [`order`](Self::order) /
/// [`error`](Self::error)).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "outcome")]
pub enum OrderResult {
    /// The order was placed. Mirrors the single-order [`OrderResponse`].
    #[serde(rename = "ok")]
    Placed {
        /// The created or updated order.
        order: Order,
        /// Immediate fills, left untyped (mirroring [`OrderResponse::fills`]).
        ///
        /// These ride through verbatim from the engine's internal fill record,
        /// whose shape (`quantity`, `maker_order_id`/`taker_order_id`, no `fee`)
        /// differs from the trade-history [`Fill`] returned by
        /// [`fetch_my_trades`](crate::Client::fetch_my_trades) (`size`,
        /// `order_id`, `fee`). Decoding these as [`Fill`] would fail, so they
        /// stay [`serde_json::Value`] pending a verified typed-fills pass.
        #[serde(default)]
        fills: Vec<serde_json::Value>,
    },
    /// The order was rejected; the rest of the batch was unaffected.
    #[serde(rename = "err")]
    Rejected {
        /// Machine-readable error code (e.g. `INSUFFICIENT_MARGIN`).
        error: String,
        /// Human-readable error message.
        message: String,
    },
}

impl OrderResult {
    /// Whether this entry is the [`Placed`](Self::Placed) (order-accepted)
    /// variant.
    pub fn succeeded(&self) -> bool {
        matches!(self, OrderResult::Placed { .. })
    }

    /// The placed [`Order`], or `None` if this entry was rejected.
    pub fn order(&self) -> Option<&Order> {
        match self {
            OrderResult::Placed { order, .. } => Some(order),
            OrderResult::Rejected { .. } => None,
        }
    }

    /// The `(error, message)` pair for a rejected entry, or `None` if it was
    /// placed.
    pub fn error(&self) -> Option<(&str, &str)> {
        match self {
            OrderResult::Rejected { error, message } => Some((error, message)),
            OrderResult::Placed { .. } => None,
        }
    }
}

/// Result of a deposit (`POST /account/deposit`).
#[derive(Debug, Clone, Deserialize)]
pub struct DepositResult {
    /// Cash balance after the deposit.
    #[serde(with = "rust_decimal::serde::str")]
    pub balance: Decimal,
}

/// A withdrawal record (`GET /withdrawals`).
#[derive(Debug, Clone, Deserialize)]
pub struct Withdrawal {
    /// Exchange-assigned withdrawal identifier.
    pub id: String,
    /// Amount withdrawn.
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    /// Unix timestamp (ms) of the withdrawal.
    pub timestamp: i64,
    /// Withdrawal status, e.g. `pending`, `completed`.
    pub status: String,
}

/// Result of claiming synthetic USDX credit (`POST /account/credit`).
#[derive(Debug, Clone, Deserialize)]
pub struct CreditResult {
    /// Amount credited by this request.
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    /// Total credited so far today, against the daily limit.
    #[serde(with = "rust_decimal::serde::str")]
    pub credited_today: Decimal,
    /// Maximum credit claimable per day.
    #[serde(with = "rust_decimal::serde::str")]
    pub daily_limit: Decimal,
}

/// An account rate-limit tier override (`/admin/tiers`).
#[derive(Debug, Clone, Deserialize)]
pub struct TierOverride {
    /// Account address the override applies to.
    pub address: String,
    /// Rate-limit tier assigned to the address.
    pub tier: String,
}

/// A freshly minted, single-use WebSocket token (`POST /ws/token`).
#[derive(Debug, Clone, Deserialize)]
pub struct WsToken {
    /// The single-use token to present when opening a WebSocket connection.
    pub token: String,
}

/// How a position is collateralized.
///
/// Serializes lowercase (`cross` / `isolated`), as the margin endpoints expect,
/// and deserializes case-insensitively so a response in any casing decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarginMode {
    /// Positions share the account's whole collateral pool.
    #[serde(alias = "Cross", alias = "CROSS")]
    Cross,
    /// Each position is margined from its own isolated collateral.
    #[serde(alias = "Isolated", alias = "ISOLATED")]
    Isolated,
}

/// Result of setting a market's leverage (`POST /account/leverage`).
#[derive(Debug, Clone, Deserialize)]
pub struct LeverageUpdate {
    /// Market the leverage applies to, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Leverage now in effect for the market.
    pub leverage: u32,
}

/// Result of setting a market's margin mode (`POST /account/margin-mode`).
#[derive(Debug, Clone, Deserialize)]
pub struct MarginModeUpdate {
    /// Market the margin mode applies to, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Margin mode now in effect for the market.
    pub margin_mode: MarginMode,
}

/// Whether an isolated-margin adjustment adds collateral to a position or
/// removes it (`POST /account/margin`).
///
/// Serializes lowercase (`add` / `remove`, as the margin endpoint expects) and
/// deserializes case-insensitively so a response in any casing decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarginDirection {
    /// Add collateral to the position's isolated margin.
    #[serde(alias = "Add", alias = "ADD")]
    Add,
    /// Remove collateral from the position's isolated margin.
    #[serde(alias = "Remove", alias = "REMOVE")]
    Remove,
}

/// Result of adjusting a position's isolated margin (`POST /account/margin`).
#[derive(Debug, Clone, Deserialize)]
pub struct MarginAdjustment {
    /// Market the adjustment applies to, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Isolated margin now allocated to the position after the adjustment.
    #[serde(with = "rust_decimal::serde::str")]
    pub allocated_margin: Decimal,
    /// Account collateral remaining after the adjustment.
    #[serde(with = "rust_decimal::serde::str")]
    pub collateral: Decimal,
}

/// Fields to change on an existing order (`PUT /orders/{id}`), an atomic
/// server-side cancel-replace.
///
/// Build one with [`AmendOrder::new`] and set only the fields you want to
/// change; unset (`None`) fields are omitted from the request and left
/// untouched on the order. [`Client::amend_order`](crate::Client::amend_order)
/// rejects an amend with no changes before it leaves the client.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AmendOrder {
    /// New limit price, if changing it.
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "rust_decimal::serde::str_option"
    )]
    pub price: Option<Decimal>,
    /// New order size, if changing it.
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "rust_decimal::serde::str_option"
    )]
    pub quantity: Option<Decimal>,
    /// New time-in-force policy, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<TimeInForce>,
    /// New client order id to assign to the replacement order, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
}

impl AmendOrder {
    /// An empty amend. Chain the setters to specify what changes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a new limit price.
    pub fn price(mut self, price: Decimal) -> Self {
        self.price = Some(price);
        self
    }

    /// Set a new order size.
    pub fn quantity(mut self, quantity: Decimal) -> Self {
        self.quantity = Some(quantity);
        self
    }

    /// Set a new time-in-force policy.
    pub fn time_in_force(mut self, time_in_force: TimeInForce) -> Self {
        self.time_in_force = Some(time_in_force);
        self
    }

    /// Assign a new client order id to the replacement order.
    pub fn client_order_id(mut self, client_order_id: impl Into<String>) -> Self {
        self.client_order_id = Some(client_order_id.into());
        self
    }

    /// Whether any field would actually change. Used to reject a no-op amend
    /// before sending it.
    pub(crate) fn has_changes(&self) -> bool {
        self.price.is_some()
            || self.quantity.is_some()
            || self.time_in_force.is_some()
            || self.client_order_id.is_some()
    }
}

/// A funding payment booked against the account (`GET /funding-payments`).
#[derive(Debug, Clone, Deserialize)]
pub struct FundingPayment {
    /// Market the payment relates to, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Amount paid (negative) or received (positive), in the quote asset.
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    /// Funding rate applied for this payment, when reported.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub funding_rate: Option<Decimal>,
    /// Unix timestamp (ms) the funding was applied.
    pub timestamp: i64,
}

/// A request to move collateral between accounts (`POST /transfers`), e.g.
/// to or from a sub-account. Construct with [`TransferRequest::new`].
#[derive(Debug, Clone, Serialize)]
pub struct TransferRequest {
    /// Account id to debit (the source).
    pub from_account: String,
    /// Account id to credit (the destination).
    pub to_account: String,
    /// Amount of collateral to move; must be positive.
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
}

impl TransferRequest {
    /// A transfer of `amount` from `from_account` to `to_account`.
    pub fn new(
        from_account: impl Into<String>,
        to_account: impl Into<String>,
        amount: Decimal,
    ) -> Self {
        Self {
            from_account: from_account.into(),
            to_account: to_account.into(),
            amount,
        }
    }
}

/// A collateral transfer record (`GET /transfers`, `POST /transfers`).
#[derive(Debug, Clone, Deserialize)]
pub struct Transfer {
    /// Exchange-assigned transfer identifier.
    pub id: String,
    /// Account that was debited.
    pub from_account: String,
    /// Account that was credited.
    pub to_account: String,
    /// Amount moved, in the quote asset.
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    /// Unix timestamp (ms) of the transfer.
    pub timestamp: i64,
    /// Transfer status, e.g. `pending`, `completed`.
    #[serde(default)]
    pub status: String,
}

/// A sub-account belonging to the authenticated master account
/// (`GET`/`POST /sub-accounts`).
#[derive(Debug, Clone, Deserialize)]
pub struct SubAccount {
    /// Exchange-assigned sub-account identifier.
    pub account_id: String,
    /// Human-readable label, if one was set.
    #[serde(default)]
    pub label: String,
    /// Sub-account equity, when reported.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub equity: Option<Decimal>,
}

/// One counterparty's forced closure within an ADL settlement. All numeric
/// fields are sent as decimal strings.
#[derive(Debug, Clone, Deserialize)]
pub struct AdlClosure {
    /// 0x-prefixed address of the counterparty whose position was closed.
    pub account_id: String,
    /// Size of the position that was forcibly closed.
    #[serde(with = "rust_decimal::serde::str")]
    pub position_closed: Decimal,
    /// Collateral settled to this counterparty for the forced closure.
    #[serde(with = "rust_decimal::serde::str")]
    pub settlement_amount: Decimal,
}

/// A single auto-deleveraging settlement event (v0.21). Emitted when the
/// insurance fund is depleted and opposite-side positions are closed to absorb
/// bad debt. Returned by the market and account ADL history endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct AdlEvent {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// 0x-prefixed bankrupt account.
    pub target_account: String,
    /// Bankruptcy price at which the target's position was settled.
    #[serde(with = "rust_decimal::serde::str")]
    pub bankruptcy_price: Decimal,
    /// Bad debt absorbed by the insurance fund before counterparties were closed.
    #[serde(with = "rust_decimal::serde::str")]
    pub bad_debt_absorbed_by_fund: Decimal,
    /// Opposite-side positions closed to absorb the bankrupt account's debt.
    #[serde(default)]
    pub counterparty_closures: Vec<AdlClosure>,
    /// Engine event sequence number.
    pub sequence: u64,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// Response to EIP-191 session login (`POST /auth/login`).
///
/// The session token authenticates `/keys` management only; for trading, mint
/// an HMAC API key and use [`Config::api_key`](crate::Config::api_key). Tokens
/// expire after 24 hours — this SDK does not refresh them. The token is held in
/// a [`SecretString`] (zeroized on drop) and the [`Debug`] impl redacts it so it
/// cannot leak through `{:?}`; `address` (the recovered public address) is shown.
#[derive(Deserialize)]
pub struct LoginResponse {
    /// Session bearer token (64-char hex). Kept secret; expose with
    /// [`secrecy::ExposeSecret`] to pass to
    /// [`Config::session_token`](crate::Config::session_token).
    pub token: SecretString,
    /// Ethereum address recovered from the login signature (`0x`-prefixed).
    pub address: String,
}

impl fmt::Debug for LoginResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginResponse")
            .field("token", &"<redacted>")
            .field("address", &self.address)
            .finish()
    }
}

/// Response to EIP-712 agent registration (`POST /agents/register`).
#[derive(Debug, Clone, Deserialize)]
pub struct AgentRegistered {
    /// The registered agent's address (`0x`-prefixed).
    pub agent_address: String,
    /// Expiry as Unix milliseconds.
    pub expires_at: u64,
}

/// Cancel-on-disconnect (COD) status for the authenticated account
/// (`GET /api/v1/account/cancel-on-disconnect`).
#[derive(Debug, Clone, Deserialize)]
pub struct CancelOnDisconnectStatus {
    /// The account's own COD opt-in setting.
    pub enabled: bool,
    /// Whether COD will actually fire: the account opt-in AND the exchange-side
    /// feature switch. When `enabled` is true but `active` is false, the exchange
    /// has the feature switched off and no cancel fires on disconnect.
    pub active: bool,
    /// Seconds the exchange waits after the last `/ws` disconnect before
    /// cancelling; a reconnect within the window disarms the cancel. `None` when
    /// the feature is unavailable on this deployment.
    #[serde(default)]
    pub grace_secs: Option<i64>,
}

/// A bridgeable asset on a specific chain (part of [`BridgeChainAssets`]).
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeAsset {
    /// Asset symbol; Phase A supports `USDC` and `USDX` only.
    pub symbol: String,
    /// On-chain token decimals for this asset on this chain.
    pub decimals: u32,
    /// Minimum amount accepted for a single deposit.
    #[serde(with = "rust_decimal::serde::str")]
    pub min_amount: Decimal,
    /// Block confirmations required before a deposit is credited.
    pub confirmations: u32,
    /// Flat fee charged in units of the asset (may be `"0"`); `None` when the
    /// spec omits it.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub fee: Option<Decimal>,
    /// `0x` token contract address on the chain; `None` for a chain-native
    /// representation.
    #[serde(default)]
    pub contract_address: Option<String>,
}

/// Bridgeable assets for one chain (part of [`BridgeAssetsResponse`]).
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeChainAssets {
    /// Chain identifier, e.g. `ethereum` or `base`.
    pub chain: String,
    /// EVM chain ID, when applicable.
    #[serde(default)]
    pub chain_id: Option<i64>,
    /// Assets that can be deposited from this chain.
    #[serde(default)]
    pub deposit_assets: Vec<BridgeAsset>,
    /// Assets that can be withdrawn to this chain (withdrawal endpoints are a
    /// later phase; this lists the eventual capability).
    #[serde(default)]
    pub withdraw_assets: Vec<BridgeAsset>,
}

/// Supported bridge chains and their deposit/withdraw assets
/// (`GET /api/v1/bridge/assets`).
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeAssetsResponse {
    /// One entry per supported chain.
    #[serde(default)]
    pub chains: Vec<BridgeChainAssets>,
}

/// A per-account deposit address on a specific chain
/// (`GET`/`POST /api/v1/bridge/deposit-addresses`).
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeDepositAddress {
    /// Deposit address on `chain` for the authenticated account. Sending a
    /// supported asset here credits the account.
    pub address: String,
    /// Chain this address belongs to.
    pub chain: String,
    /// Assets creditable via this address.
    #[serde(default)]
    pub accepts: Vec<String>,
    /// `0x`-prefixed Nexus account the address credits.
    #[serde(default)]
    pub account_id: String,
    /// Unix timestamp (ms) the address was created.
    #[serde(default)]
    pub created_at: i64,
}

/// A cross-chain deposit tracked by the watcher
/// (`GET /api/v1/bridge/deposits`, `GET /api/v1/bridge/deposits/{id}`).
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeDeposit {
    /// Opaque, stable deposit identifier.
    pub id: String,
    /// `0x`-prefixed Nexus account being credited.
    #[serde(default)]
    pub account_id: String,
    /// Source chain.
    pub chain: String,
    /// Deposited asset (`USDC` or `USDX`).
    pub asset: String,
    /// Deposit amount in units of `asset`.
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    /// Deposit address the funds arrived at.
    #[serde(default)]
    pub address: String,
    /// Lifecycle: `detected` → `confirming` → `credited` | `failed`.
    pub status: String,
    /// Confirmations observed so far; `None` before the tx is seen on chain.
    #[serde(default)]
    pub confirmations: Option<u32>,
    /// Confirmations required before crediting.
    #[serde(default)]
    pub required_confirmations: Option<u32>,
    /// Source-chain transaction hash; `None` until detected.
    #[serde(default)]
    pub tx_hash: Option<String>,
    /// Unix timestamp (ms) the deposit was first tracked.
    #[serde(default)]
    pub created_at: i64,
    /// Unix timestamp (ms) the deposit was last updated.
    #[serde(default)]
    pub updated_at: i64,
    /// Unix timestamp (ms) the deposit was credited; `None` until `status` is
    /// `credited`.
    #[serde(default)]
    pub credited_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn created_api_key_debug_redacts_secret() {
        let key: CreatedApiKey = serde_json::from_value(serde_json::json!({
            "key_id": "nx_abc",
            "secret": "supersecrethexvalue",
            "tier": "Pro",
        }))
        .unwrap();
        // The secret round-trips into the field for the caller to persist,
        // but must never appear in the Debug rendering.
        assert_eq!(key.secret.expose_secret(), "supersecrethexvalue");
        let rendered = format!("{key:?}");
        assert!(
            !rendered.contains("supersecrethexvalue"),
            "secret leaked: {rendered}"
        );
        assert!(rendered.contains("nx_abc"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn created_api_key_tier_is_optional() {
        let key: CreatedApiKey = serde_json::from_value(serde_json::json!({
            "key_id": "nx_abc",
            "secret": "s",
        }))
        .unwrap();
        assert!(key.tier.is_none());
    }

    #[test]
    fn time_in_force_serde_wire_values() {
        // GTC/IOC/FOK take the container's UPPERCASE rename; PostOnly overrides it
        // to the PascalCase `PostOnly` the engine expects (a bare UPPERCASE rename
        // would emit `POSTONLY` and be rejected). Round-trip each so a rename slip
        // is caught in both directions.
        for (tif, wire) in [
            (TimeInForce::Gtc, "\"GTC\""),
            (TimeInForce::Ioc, "\"IOC\""),
            (TimeInForce::Fok, "\"FOK\""),
            (TimeInForce::PostOnly, "\"PostOnly\""),
        ] {
            assert_eq!(
                serde_json::to_string(&tif).unwrap(),
                wire,
                "serialize {tif:?}"
            );
            assert_eq!(
                serde_json::from_str::<TimeInForce>(wire).unwrap(),
                tif,
                "deserialize {wire}"
            );
        }
    }

    #[test]
    fn login_response_debug_redacts_token() {
        let resp: LoginResponse = serde_json::from_value(serde_json::json!({
            "token": "deadbeefsessiontoken", "address": "0xabc",
        }))
        .unwrap();
        assert_eq!(resp.token.expose_secret(), "deadbeefsessiontoken");
        let rendered = format!("{resp:?}");
        assert!(
            !rendered.contains("deadbeefsessiontoken"),
            "leaked: {rendered}"
        );
        assert!(rendered.contains("0xabc") && rendered.contains("<redacted>"));
    }

    #[test]
    fn agent_info_parses_camel_case_and_defaults() {
        let agent: AgentInfo = serde_json::from_value(serde_json::json!({
            "address": "0xagent",
            "expiresAt": 1_776_033_900_000i64,
            "registeredAt": 1_776_000_000_000i64,
            "label": "my-bot",
        }))
        .unwrap();
        assert_eq!(agent.address, "0xagent");
        assert_eq!(agent.expires_at, 1_776_033_900_000);
        assert_eq!(agent.label.as_deref(), Some("my-bot"));

        // Optional fields default when the server omits them.
        let slim: AgentInfo =
            serde_json::from_value(serde_json::json!({ "address": "0xagent" })).unwrap();
        assert_eq!(slim.registered_at, 0);
        assert!(slim.label.is_none());
    }

    #[test]
    fn adl_event_parses_nested_closures() {
        let ev: AdlEvent = serde_json::from_value(serde_json::json!({
            "market_id": "BTC-USDX-PERP", "target_account": "0xbankrupt",
            "bankruptcy_price": "49999.5", "bad_debt_absorbed_by_fund": "12.25",
            "counterparty_closures": [
                { "account_id": "0xcp", "position_closed": "0.5", "settlement_amount": "25000" }
            ],
            "sequence": 42, "timestamp": 1_776_033_900_000i64,
        }))
        .unwrap();
        assert_eq!(ev.market_id, "BTC-USDX-PERP");
        assert_eq!(ev.bankruptcy_price.to_string(), "49999.5");
        assert_eq!(ev.counterparty_closures.len(), 1);
        assert_eq!(
            ev.counterparty_closures[0].position_closed.to_string(),
            "0.5"
        );
        assert_eq!(ev.sequence, 42);

        // counterparty_closures defaults to empty when the server omits it.
        let no_closures: AdlEvent = serde_json::from_value(serde_json::json!({
            "market_id": "BTC-USDX-PERP", "target_account": "0xbankrupt",
            "bankruptcy_price": "1", "bad_debt_absorbed_by_fund": "0",
            "sequence": 1, "timestamp": 1i64,
        }))
        .unwrap();
        assert!(no_closures.counterparty_closures.is_empty());
    }
}
