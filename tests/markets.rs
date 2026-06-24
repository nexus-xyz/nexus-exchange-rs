//! Tier-2 order-precision helpers: tick/lot rounding and limit validation.

use nexus_exchange::types::Market;
use nexus_exchange::{OrderError, Rounding};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// A market with friendly round-number rules for assertions.
fn market() -> Market {
    serde_json::from_value(serde_json::json!({
        "market_id": "BTC-USDX-PERP",
        "base_asset": "BTC",
        "quote_asset": "USDX",
        "tick_size": "0.5",
        "lot_size": "0.001",
        "min_order_size": "0.01",
        "max_order_size": "100",
        "initial_margin_rate": "0.05",
        "maintenance_margin_rate": "0.03",
        "max_leverage": 20
    }))
    .unwrap()
}

#[test]
fn round_price_snaps_to_tick() {
    let m = market();
    // 50000.30 sits between ticks 50000.0 and 50000.5.
    assert_eq!(m.round_price(dec!(50000.30), Rounding::Down), dec!(50000.0));
    assert_eq!(m.round_price(dec!(50000.30), Rounding::Up), dec!(50000.5));
    assert_eq!(
        m.round_price(dec!(50000.30), Rounding::Nearest),
        dec!(50000.5)
    );
    assert_eq!(
        m.round_price(dec!(50000.24), Rounding::Nearest),
        dec!(50000.0)
    );
    // Ties round away from zero.
    assert_eq!(
        m.round_price(dec!(50000.25), Rounding::Nearest),
        dec!(50000.5)
    );
    // Already on-tick passes through.
    assert_eq!(m.round_price(dec!(50000.5), Rounding::Down), dec!(50000.5));
}

#[test]
fn round_size_snaps_to_lot() {
    let m = market();
    assert_eq!(m.round_size(dec!(1.23456), Rounding::Down), dec!(1.234));
    assert_eq!(m.round_size(dec!(1.23456), Rounding::Up), dec!(1.235));
    assert_eq!(m.round_size(dec!(1.2345), Rounding::Nearest), dec!(1.235));
}

#[test]
fn round_is_sign_symmetric_for_negatives() {
    let m = market();
    // `Down` truncates toward zero, `Up` rounds away from zero — both directions
    // for negatives too (regression guard against floor/ceil toward ∓∞).
    assert_eq!(
        m.round_price(dec!(-50000.3), Rounding::Down),
        dec!(-50000.0)
    );
    assert_eq!(m.round_price(dec!(-50000.3), Rounding::Up), dec!(-50000.5));
    assert_eq!(m.round_size(dec!(-1.23456), Rounding::Down), dec!(-1.234));
    assert_eq!(m.round_size(dec!(-1.23456), Rounding::Up), dec!(-1.235));
    // Ties still go away from zero on the negative side.
    assert_eq!(
        m.round_price(dec!(-50000.25), Rounding::Nearest),
        dec!(-50000.5)
    );
}

#[test]
fn zero_increment_passes_through() {
    // tick_size / lot_size of 0 means "no grid": values pass through untouched
    // instead of dividing by zero.
    let m: Market = serde_json::from_value(serde_json::json!({
        "market_id": "BTC-USDX-PERP",
        "base_asset": "BTC",
        "quote_asset": "USDX",
        "tick_size": "0",
        "lot_size": "0",
        "min_order_size": "0",
        "max_order_size": "1000000",
        "initial_margin_rate": "0.05",
        "maintenance_margin_rate": "0.03",
        "max_leverage": 20
    }))
    .unwrap();
    assert_eq!(m.round_price(dec!(50000.3), Rounding::Down), dec!(50000.3));
    assert_eq!(
        m.round_size(dec!(1.23456), Rounding::Nearest),
        dec!(1.23456)
    );
}

#[test]
fn round_does_not_panic_on_extreme_magnitude() {
    let m = market();
    // `Decimal::MAX / 0.5` overflows; the helper must pass the value through
    // rather than panicking on arbitrary public input.
    assert_eq!(m.round_price(Decimal::MAX, Rounding::Down), Decimal::MAX);
    assert_eq!(m.round_size(Decimal::MIN, Rounding::Up), Decimal::MIN);
}

#[test]
fn round_result_is_clean_scale() {
    let m = market();
    // Re-multiplying 100002 * 0.5 must not leave a noisy "50001.0" scale.
    assert_eq!(
        m.round_price(dec!(50001.0), Rounding::Down).to_string(),
        "50001"
    );
}

#[test]
fn validate_accepts_a_well_formed_order() {
    let m = market();
    assert!(m.validate_order(dec!(50000.5), dec!(1.234)).is_ok());
}

#[test]
fn validate_rejects_off_tick_price() {
    let m = market();
    let err = m.validate_order(dec!(50000.3), dec!(1.0)).unwrap_err();
    assert_eq!(
        err,
        OrderError::PriceNotOnTick {
            price: dec!(50000.3),
            tick_size: dec!(0.5),
        }
    );
}

#[test]
fn validate_rejects_off_lot_size() {
    let m = market();
    let err = m.validate_order(dec!(50000.5), dec!(1.2345)).unwrap_err();
    assert_eq!(
        err,
        OrderError::SizeNotOnLot {
            size: dec!(1.2345),
            lot_size: dec!(0.001),
        }
    );
}

#[test]
fn validate_rejects_size_below_min_and_above_max() {
    let m = market();
    assert_eq!(
        m.validate_order(dec!(50000.5), dec!(0.001)).unwrap_err(),
        OrderError::SizeBelowMin {
            size: dec!(0.001),
            min: dec!(0.01),
        }
    );
    assert_eq!(
        m.validate_order(dec!(50000.5), dec!(101)).unwrap_err(),
        OrderError::SizeAboveMax {
            size: dec!(101),
            max: dec!(100),
        }
    );
}

#[test]
fn validate_rejects_non_positive_values() {
    let m = market();
    assert_eq!(
        m.validate_order(Decimal::ZERO, dec!(1.0)).unwrap_err(),
        OrderError::NonPositivePrice(Decimal::ZERO)
    );
    assert_eq!(
        m.validate_order(dec!(50000.5), dec!(-1.0)).unwrap_err(),
        OrderError::NonPositiveSize(dec!(-1.0))
    );
}

#[test]
fn normalize_order_rounds_then_validates() {
    let m = market();
    // Off-grid inputs get snapped and pass.
    let (price, size) = m
        .normalize_order(dec!(50000.34), dec!(1.23456), Rounding::Nearest)
        .unwrap();
    assert_eq!(price, dec!(50000.5));
    assert_eq!(size, dec!(1.235));
}

#[test]
fn normalize_order_surfaces_limit_violation_rounding_cannot_fix() {
    let m = market();
    // 0.004 rounds to a valid lot multiple but is still below the 0.01 minimum.
    let err = m
        .normalize_order(dec!(50000.5), dec!(0.004), Rounding::Nearest)
        .unwrap_err();
    assert_eq!(
        err,
        OrderError::SizeBelowMin {
            size: dec!(0.004),
            min: dec!(0.01),
        }
    );
}

#[test]
fn order_error_converts_into_crate_error() {
    let m = market();
    let err: nexus_exchange::Error = m
        .validate_order(dec!(50000.3), dec!(1.0))
        .unwrap_err()
        .into();
    assert!(matches!(
        err,
        nexus_exchange::Error::Terminal(nexus_exchange::TerminalError::OrderValidation(_))
    ));
}
