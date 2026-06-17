//! Order-precision helpers — round price/size to a market's tick/lot grid and
//! validate orders against its limits *before* submitting.
//!
//! The exchange rejects orders whose price isn't a multiple of `tick_size`,
//! whose size isn't a multiple of `lot_size`, or whose size falls outside
//! `[min_order_size, max_order_size]`. Those rejections come back as opaque API
//! errors well after you've built the order, so this module lets you snap and
//! check locally first. Trading rules come straight from
//! [`Client::fetch_markets`](crate::Client::fetch_markets).

use crate::types::Market;
use rust_decimal::{Decimal, RoundingStrategy};
use thiserror::Error;

/// How to snap a value that falls between two valid grid points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    /// Toward zero (truncate). Safe default for sizes — never rounds *up* into
    /// more risk than you asked for.
    Down,
    /// Away from zero.
    Up,
    /// To the nearest grid point; ties go away from zero.
    Nearest,
}

/// Why an order would be rejected by the exchange's trading rules.
///
/// Surfaced through [`crate::Error::InvalidOrder`] when it crosses an SDK
/// boundary, so callers can use the crate `Result` throughout.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum OrderError {
    /// Price was zero or negative.
    #[error("price must be positive, got {0}")]
    NonPositivePrice(Decimal),

    /// Size was zero or negative.
    #[error("size must be positive, got {0}")]
    NonPositiveSize(Decimal),

    /// Price is not an exact multiple of `tick_size`.
    #[error("price {price} is not a multiple of tick size {tick_size}")]
    PriceNotOnTick {
        /// The offending price.
        price: Decimal,
        /// The market's tick size.
        tick_size: Decimal,
    },

    /// Size is not an exact multiple of `lot_size`.
    #[error("size {size} is not a multiple of lot size {lot_size}")]
    SizeNotOnLot {
        /// The offending size.
        size: Decimal,
        /// The market's lot size.
        lot_size: Decimal,
    },

    /// Size is below `min_order_size`.
    #[error("size {size} is below the minimum order size {min}")]
    SizeBelowMin {
        /// The offending size.
        size: Decimal,
        /// The market's minimum order size.
        min: Decimal,
    },

    /// Size is above `max_order_size`.
    #[error("size {size} is above the maximum order size {max}")]
    SizeAboveMax {
        /// The offending size.
        size: Decimal,
        /// The market's maximum order size.
        max: Decimal,
    },
}

/// Snap `value` to the nearest multiple of `increment` per `mode`.
///
/// A zero (or absent) increment means "no grid" — the value passes through
/// unchanged rather than dividing by zero.
fn round_to_increment(value: Decimal, increment: Decimal, mode: Rounding) -> Decimal {
    if increment.is_zero() {
        return value;
    }
    let steps = value / increment;
    let snapped = match mode {
        Rounding::Down => steps.floor(),
        Rounding::Up => steps.ceil(),
        Rounding::Nearest => {
            steps.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        }
    };
    // Re-multiplying can leave trailing-zero scale (e.g. `100.10`); normalize so
    // the result reads like a hand-typed price.
    (snapped * increment).normalize()
}

impl Market {
    /// Round `price` to this market's `tick_size`.
    ///
    /// ```
    /// # use rust_decimal::Decimal;
    /// # use nexus_exchange::markets::Rounding;
    /// # let market: nexus_exchange::types::Market = serde_json::from_value(serde_json::json!({
    /// #   "market_id":"BTC-USDX-PERP","base_asset":"BTC","quote_asset":"USDX",
    /// #   "tick_size":"0.5","lot_size":"0.001","min_order_size":"0.001","max_order_size":"100",
    /// #   "initial_margin_rate":"0.05","maintenance_margin_rate":"0.03","max_leverage":20})).unwrap();
    /// let p = market.round_price(Decimal::new(500123, 2), Rounding::Down); // 5001.23
    /// assert_eq!(p, Decimal::new(5001, 0)); // snapped down to 5001.0
    /// ```
    pub fn round_price(&self, price: Decimal, mode: Rounding) -> Decimal {
        round_to_increment(price, self.tick_size, mode)
    }

    /// Round `size` to this market's `lot_size`.
    pub fn round_size(&self, size: Decimal, mode: Rounding) -> Decimal {
        round_to_increment(size, self.lot_size, mode)
    }

    /// Validate a `(price, size)` pair against this market's trading rules
    /// without rounding it first. Returns the first violation found.
    ///
    /// Checks, in order: price positive and on-tick; size positive, on-lot, and
    /// within `[min_order_size, max_order_size]`.
    pub fn validate_order(
        &self,
        price: Decimal,
        size: Decimal,
    ) -> std::result::Result<(), OrderError> {
        if price <= Decimal::ZERO {
            return Err(OrderError::NonPositivePrice(price));
        }
        if !self.tick_size.is_zero() && !(price % self.tick_size).is_zero() {
            return Err(OrderError::PriceNotOnTick {
                price,
                tick_size: self.tick_size,
            });
        }
        if size <= Decimal::ZERO {
            return Err(OrderError::NonPositiveSize(size));
        }
        if !self.lot_size.is_zero() && !(size % self.lot_size).is_zero() {
            return Err(OrderError::SizeNotOnLot {
                size,
                lot_size: self.lot_size,
            });
        }
        if size < self.min_order_size {
            return Err(OrderError::SizeBelowMin {
                size,
                min: self.min_order_size,
            });
        }
        if size > self.max_order_size {
            return Err(OrderError::SizeAboveMax {
                size,
                max: self.max_order_size,
            });
        }
        Ok(())
    }

    /// Round `price` to tick and `size` to lot (both with `mode`), then validate
    /// the result. Returns the submit-ready `(price, size)` or the first
    /// violation that rounding couldn't fix (e.g. size still below the minimum).
    ///
    /// This is the one call to make right before building an order.
    pub fn normalize_order(
        &self,
        price: Decimal,
        size: Decimal,
        mode: Rounding,
    ) -> std::result::Result<(Decimal, Decimal), OrderError> {
        let price = self.round_price(price, mode);
        let size = self.round_size(size, mode);
        self.validate_order(price, size)?;
        Ok((price, size))
    }
}
