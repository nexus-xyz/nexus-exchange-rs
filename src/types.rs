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
//! [`Ticker`], [`Trade`], [`MarketSummary`] (`mark_price`, `volume_24h`),
//! [`OrderBook`] / [`PriceLevel`], and [`Ohlcv`].
//!
//! The clean fix is on the API side: if these endpoints emitted decimal strings
//! like the others, the SDK could use the `str` adapter everywhere and every
//! field would be exact. That change is tracked separately; until then this
//! module documents the gap rather than papering over it.

pub use rust_decimal::Decimal;
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
/// `mark_price` and `volume_24h` arrive as JSON numbers via the `float` adapter
/// and may carry `f64` rounding artifacts — see the [module precision
/// note](crate::types#precision-of-float-adapter-fields).
#[derive(Debug, Clone, Deserialize)]
pub struct MarketSummary {
    /// Market identifier, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Mark price as a JSON number; `null` for a halted market with no recent
    /// mark (the spec types this `["number","null"]`).
    #[serde(with = "rust_decimal::serde::float_option")]
    pub mark_price: Option<Decimal>,
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

/// Indexer health/status snapshot (`GET /health`). Unknown fields are ignored,
/// so this stays forward-compatible as the snapshot grows.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthStatus {
    /// Total events the indexer has received.
    #[serde(default)]
    pub events_received: u64,
    /// Total fills the indexer has processed.
    #[serde(default)]
    pub fills_total: u64,
    /// Seconds since the indexer started.
    #[serde(default)]
    pub uptime_seconds: u64,
    /// Whether the indexer is currently connected to its upstream feed.
    #[serde(default)]
    pub connected: bool,
    /// Coarse health state, when reported (e.g. `healthy`, `degraded`).
    #[serde(default)]
    pub health: Option<String>,
}

/// An API key associated with the authenticated session (`GET /keys`).
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    /// Opaque identifier for the key.
    pub key_id: String,
    /// Rate-limit tier this key resolves to.
    pub tier: String,
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

/// An open position.
#[derive(Debug, Clone, Deserialize)]
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
        }
    }

    /// Attach a caller-assigned client order id, consuming and returning `self`
    /// so it chains off [`limit`](Self::limit) / [`market`](Self::market).
    pub fn with_client_order_id(mut self, client_order_id: impl Into<String>) -> Self {
        self.client_order_id = Some(client_order_id.into());
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
    /// Unix timestamp (ms) when the order was created.
    #[serde(default)]
    pub created_at: i64,
    /// Unix timestamp (ms) when the order was last updated.
    #[serde(default)]
    pub updated_at: i64,
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
