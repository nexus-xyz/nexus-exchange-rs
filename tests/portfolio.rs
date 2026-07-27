//! Portfolio-parity surface (ENG-6457): portfolio time series, enriched
//! `Position` risk fields, `withdrawable` + consolidated account state, and
//! `/account/fees`.

use nexus_exchange::types::PortfolioWindow;
use nexus_exchange::{Client, Config};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

/// A position payload carrying every enriched risk field populated.
fn enriched_position() -> serde_json::Value {
    serde_json::json!({
        "market_id": "BTC-USDX-PERP", "side": "long", "size": "0.5",
        "entry_price": "50000", "unrealized_pnl": "12.34", "realized_pnl": "1.00",
        "liquidation_price": "40000",
        "leverage": 5.0, "leverage_error": null,
        "notional_value": "25006.17", "notional_value_error": null,
        "roe": "0.0049", "roe_error": null,
        "margin_used": "2500.61", "margin_used_error": null,
        "max_leverage": 20, "max_leverage_error": null,
        "funding_paid": "-1.25"
    })
}

/// A summary payload with `withdrawable` present.
fn summary_json() -> serde_json::Value {
    serde_json::json!({
        "collateral": "10000.00",
        "total_equity": "10012.34",
        "total_unrealized_pnl": "12.34",
        "total_realized_pnl_24h": "1.00",
        "total_volume_24h": "125000.00",
        "open_positions_count": 1,
        "open_orders_count": 2,
        "margin_used": "2500.61",
        "available_margin": "7511.73",
        "withdrawable": "7511.73"
    })
}

// --- Portfolio time series -------------------------------------------------

#[tokio::test]
async fn fetch_portfolio_history_parses_and_sends_window_and_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/portfolio-history"))
        .and(header("x-api-key", "nx_test"))
        .and(query_param("window", "week"))
        .and(query_param("limit", "168"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "window": "week",
            "cadence_ms": 3600000i64,
            "points": [
                { "timestamp_ms": 1776033900000i64, "equity": "10000.00",
                  "pnl": "0", "volume": "0" },
                { "timestamp_ms": 1776037500000i64, "equity": "10012.34",
                  "pnl": "12.34", "volume": "125000.00" }
            ]
        })))
        .mount(&server)
        .await;

    let history = authed(server.uri())
        .fetch_portfolio_history(Some(PortfolioWindow::Week), Some(168))
        .await
        .unwrap();

    assert_eq!(history.window, PortfolioWindow::Week);
    assert_eq!(history.cadence_ms, 3_600_000);
    assert_eq!(history.points.len(), 2);
    // Oldest first, and the monetary fields are exact decimal strings.
    assert_eq!(history.points[0].pnl.to_string(), "0");
    assert_eq!(history.points[1].equity.to_string(), "10012.34");
    assert_eq!(history.points[1].volume.to_string(), "125000.00");
}

#[tokio::test]
async fn fetch_portfolio_history_omits_unset_params() {
    // No window / limit must send NO query params, so the server applies its own
    // `day` default rather than the SDK guessing one.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/portfolio-history"))
        .and(query_param_is_missing("window"))
        .and(query_param_is_missing("limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "window": "day", "cadence_ms": 300000i64, "points": []
        })))
        .mount(&server)
        .await;

    let history = authed(server.uri())
        .fetch_portfolio_history(None, None)
        .await
        .unwrap();

    assert_eq!(history.window, PortfolioWindow::Day);
    assert!(history.points.is_empty());
}

#[tokio::test]
async fn fetch_portfolio_history_rejects_zero_limit_locally() {
    // The spec's `limit` minimum is 1. A 0 must be rejected before the request is
    // signed or sent — the mock server mounts no route, so any request 404s and
    // would surface as a different error than the local invalid-request one.
    let server = MockServer::start().await;
    let err = authed(server.uri())
        .fetch_portfolio_history(Some(PortfolioWindow::Day), Some(0))
        .await
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("limit"),
        "expected a local limit rejection, got: {err}"
    );
    // Nothing left the client.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn fetch_portfolio_history_passes_oversized_limit_through() {
    // Above the window capacity the server CLAMPS rather than rejecting, so the
    // SDK must forward the value instead of second-guessing the cap.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/portfolio-history"))
        .and(query_param("limit", "5000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "window": "all", "cadence_ms": 86400000i64, "points": []
        })))
        .mount(&server)
        .await;

    let history = authed(server.uri())
        .fetch_portfolio_history(Some(PortfolioWindow::All), Some(5000))
        .await
        .unwrap();
    // The served window is read back from the response, not assumed.
    assert_eq!(history.window, PortfolioWindow::All);
}

#[tokio::test]
async fn portfolio_window_wire_values_and_case_insensitive_decode() {
    assert_eq!(PortfolioWindow::Day.as_str(), "day");
    assert_eq!(PortfolioWindow::Week.as_str(), "week");
    assert_eq!(PortfolioWindow::Month.as_str(), "month");
    assert_eq!(PortfolioWindow::All.as_str(), "all");
    assert_eq!(PortfolioWindow::default(), PortfolioWindow::Day);
    assert_eq!(PortfolioWindow::Month.to_string(), "month");

    // Canonical lowercase plus the aliases, so an echoed value in any casing
    // decodes rather than failing the whole response.
    for (wire, want) in [
        ("day", PortfolioWindow::Day),
        ("Week", PortfolioWindow::Week),
        ("MONTH", PortfolioWindow::Month),
        ("all", PortfolioWindow::All),
    ] {
        let got: PortfolioWindow = serde_json::from_value(serde_json::json!(wire)).expect(wire);
        assert_eq!(got, want, "{wire}");
    }
    // Serializes back to the canonical lowercase form.
    assert_eq!(
        serde_json::to_value(PortfolioWindow::Week).unwrap(),
        serde_json::json!("week")
    );
    // An unknown window is a decode error, not a silent fallback to `day`.
    assert!(serde_json::from_value::<PortfolioWindow>(serde_json::json!("quarter")).is_err());
}

// --- Consolidated account state + withdrawable -----------------------------

#[tokio::test]
async fn fetch_account_state_parses_summary_and_positions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/state"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "summary": summary_json(),
            "positions": [enriched_position()]
        })))
        .mount(&server)
        .await;

    let state = authed(server.uri()).fetch_account_state().await.unwrap();

    assert_eq!(state.summary.withdrawable.unwrap().to_string(), "7511.73");
    assert_eq!(state.summary.total_equity.to_string(), "10012.34");
    assert_eq!(state.summary.open_orders_count, 2);
    // The server builds both halves from one read, so these agree by contract.
    assert_eq!(
        state.summary.open_positions_count as usize,
        state.positions.len()
    );
    assert_eq!(state.positions[0].market_id, "BTC-USDX-PERP");
}

#[tokio::test]
async fn fetch_account_summary_parses_withdrawable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/summary"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(summary_json()))
        .mount(&server)
        .await;

    let summary = authed(server.uri()).fetch_account_summary().await.unwrap();
    assert_eq!(summary.withdrawable.unwrap().to_string(), "7511.73");
    assert_eq!(summary.margin_used.to_string(), "2500.61");
    // Absent unless the early-access gate is active.
    assert!(summary.early_access_allowed.is_none());
}

#[tokio::test]
async fn account_summary_tolerates_absent_and_null_withdrawable() {
    // A server predating `withdrawable` omits it; the spec also allows an explicit
    // null. Neither may fail the whole summary decode.
    let server = MockServer::start().await;
    let mut absent = summary_json();
    absent.as_object_mut().unwrap().remove("withdrawable");
    let mut null = summary_json();
    null["withdrawable"] = serde_json::Value::Null;

    Mock::given(method("GET"))
        .and(path("/api/v1/account/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(absent))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let client = authed(server.uri());
    let summary = client.fetch_account_summary().await.unwrap();
    assert!(summary.withdrawable.is_none());
    // Available margin still decodes, so the caller isn't left with nothing.
    assert_eq!(summary.available_margin.to_string(), "7511.73");

    let server2 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(null))
        .mount(&server2)
        .await;
    let summary = authed(server2.uri()).fetch_account_summary().await.unwrap();
    assert!(summary.withdrawable.is_none());
}

#[tokio::test]
async fn withdrawable_is_never_negative_even_when_margin_is() {
    // An underwater account: the server clamps `withdrawable` to 0 while
    // `available_margin` stays negative. The SDK must surface both faithfully —
    // withdrawable is the field to gate a withdrawal on.
    let server = MockServer::start().await;
    let mut body = summary_json();
    body["available_margin"] = serde_json::json!("-250.00");
    body["withdrawable"] = serde_json::json!("0");
    Mock::given(method("GET"))
        .and(path("/api/v1/account/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let summary = authed(server.uri()).fetch_account_summary().await.unwrap();
    assert!(summary.available_margin.is_sign_negative());
    assert!(summary.withdrawable.unwrap().is_zero());
}

// --- Enriched Position risk fields -----------------------------------------

#[tokio::test]
async fn fetch_positions_parses_enriched_risk_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/positions"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([enriched_position()])),
        )
        .mount(&server)
        .await;

    let positions = authed(server.uri()).fetch_positions().await.unwrap();
    let p = &positions[0];
    assert_eq!(p.leverage.unwrap().to_string(), "5");
    assert_eq!(p.notional_value.unwrap().to_string(), "25006.17");
    assert_eq!(p.roe.unwrap().to_string(), "0.0049");
    assert_eq!(p.margin_used.unwrap().to_string(), "2500.61");
    assert_eq!(p.max_leverage, Some(20));
    // Paid-positive: negative means the position RECEIVED funding.
    assert_eq!(p.funding_paid.unwrap().to_string(), "-1.25");
    // Every field populated => no error companions.
    assert!(p.leverage_error.is_none());
    assert!(p.notional_value_error.is_none());
    assert!(p.roe_error.is_none());
    assert!(p.margin_used_error.is_none());
    assert!(p.max_leverage_error.is_none());
}

#[tokio::test]
async fn null_risk_fields_decode_to_none_with_error_reasons() {
    // The documented shape when an input isn't indexer-mirrored: the value is
    // null and the companion `*_error` says why. `None` must not be read as zero.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/positions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "market_id": "ETH-USDX-PERP", "side": "short", "size": "1",
                "entry_price": "3000", "unrealized_pnl": "0", "realized_pnl": "0",
                "leverage": null, "leverage_error": "margin_state_not_mirrored",
                "notional_value": null, "notional_value_error": "mark_price_unavailable",
                "roe": null, "roe_error": "margin_used_zero",
                "margin_used": null, "margin_used_error": "margin_rate_unavailable",
                "max_leverage": null, "max_leverage_error": "market_params_unavailable",
                "funding_paid": "0"
            }])),
        )
        .mount(&server)
        .await;

    let positions = authed(server.uri()).fetch_positions().await.unwrap();
    let p = &positions[0];
    assert!(p.leverage.is_none());
    assert_eq!(
        p.leverage_error.as_deref(),
        Some("margin_state_not_mirrored")
    );
    assert!(p.notional_value.is_none());
    assert_eq!(
        p.notional_value_error.as_deref(),
        Some("mark_price_unavailable")
    );
    assert!(p.roe.is_none());
    assert_eq!(p.roe_error.as_deref(), Some("margin_used_zero"));
    assert!(p.margin_used.is_none());
    assert_eq!(
        p.margin_used_error.as_deref(),
        Some("margin_rate_unavailable")
    );
    assert!(p.max_leverage.is_none());
    assert_eq!(
        p.max_leverage_error.as_deref(),
        Some("market_params_unavailable")
    );
    // Always sent, "0" when nothing accrued — distinct from "not computable".
    assert!(p.funding_paid.unwrap().is_zero());
}

#[tokio::test]
async fn leverage_decodes_from_integer_and_fractional_json_numbers() {
    // `leverage` is the one enriched field the API sends as a JSON *number*, and
    // an integral value may arrive as `5` rather than `5.0`. Both must decode.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "market_id": "BTC-USDX-PERP", "side": "long", "size": "1",
                "entry_price": "1", "unrealized_pnl": "0", "realized_pnl": "0",
                "leverage": 5
            },
            {
                "market_id": "ETH-USDX-PERP", "side": "long", "size": "1",
                "entry_price": "1", "unrealized_pnl": "0", "realized_pnl": "0",
                "leverage": 12.5
            }
        ])))
        .mount(&server)
        .await;

    let positions = authed(server.uri()).fetch_positions().await.unwrap();
    assert_eq!(positions[0].leverage.unwrap().to_string(), "5");
    assert_eq!(positions[1].leverage.unwrap().to_string(), "12.5");
}

#[tokio::test]
async fn positions_from_a_server_without_enriched_fields_still_decode() {
    // Rolling deploy / older server: the enriched keys are absent entirely. That
    // must degrade to None, not fail the whole positions read.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/positions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "market_id": "ETH-USDX-PERP", "side": "long", "size": "1",
                "entry_price": "3000", "unrealized_pnl": "0", "realized_pnl": "0"
            }])),
        )
        .mount(&server)
        .await;

    let positions = authed(server.uri()).fetch_positions().await.unwrap();
    let p = &positions[0];
    assert_eq!(p.market_id, "ETH-USDX-PERP");
    assert!(p.leverage.is_none());
    assert!(p.leverage_error.is_none());
    assert!(p.notional_value.is_none());
    assert!(p.margin_used.is_none());
    assert!(p.max_leverage.is_none());
    assert!(p.funding_paid.is_none());
}

#[tokio::test]
async fn fetch_balance_still_decodes_enriched_positions() {
    // `Position` is shared by /account, /positions and /account/state — the added
    // fields must not disturb the balance payload's embedded positions.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "balance": "10000.00", "collateral": "10000.00", "equity": "10012.34",
            "available_margin": "7511.73",
            "positions": [enriched_position()]
        })))
        .mount(&server)
        .await;

    let acct = authed(server.uri()).fetch_balance().await.unwrap();
    assert_eq!(acct.positions[0].max_leverage, Some(20));
    assert_eq!(
        acct.positions[0].notional_value.unwrap().to_string(),
        "25006.17"
    );
}

// --- Account fees ----------------------------------------------------------

#[tokio::test]
async fn fetch_account_fees_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/fees"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "maker_fee_bps": -2,
            "taker_fee_bps": 5,
            "tier": "base",
            "schedule": "standard",
            "volume_30d": "1250000.50",
            "volume_30d_estimated": false,
            "discounts": []
        })))
        .mount(&server)
        .await;

    let fees = authed(server.uri()).fetch_account_fees().await.unwrap();
    // Signed: a negative maker fee is a rebate, so this must not be unsigned.
    assert_eq!(fees.maker_fee_bps, -2);
    assert_eq!(fees.taker_fee_bps, 5);
    assert_eq!(fees.tier, "base");
    assert_eq!(fees.schedule, "standard");
    assert_eq!(fees.volume_30d.to_string(), "1250000.50");
    assert!(!fees.volume_30d_estimated);
    assert!(fees.discounts.is_empty());
}

#[tokio::test]
async fn account_fees_surfaces_estimated_volume_and_opaque_discounts() {
    // `volume_30d_estimated: true` means the 30d volume may UNDERCOUNT; and a
    // discount object of not-yet-specified shape must be preserved, not dropped.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/fees"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "maker_fee_bps": 0,
            "taker_fee_bps": 3,
            "tier": "vip1",
            "schedule": "fx",
            "volume_30d": "999.00",
            "volume_30d_estimated": true,
            "discounts": [{ "kind": "referral", "bps": 1 }]
        })))
        .mount(&server)
        .await;

    let fees = authed(server.uri()).fetch_account_fees().await.unwrap();
    assert!(fees.volume_30d_estimated);
    // `tier` / `schedule` are open strings: unknown values decode, not error.
    assert_eq!(fees.tier, "vip1");
    assert_eq!(fees.schedule, "fx");
    assert_eq!(fees.discounts.len(), 1);
    assert_eq!(
        fees.discounts[0].fields.get("kind"),
        Some(&serde_json::json!("referral"))
    );
}

#[tokio::test]
async fn account_fees_defaults_absent_discounts_to_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account/fees"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "maker_fee_bps": 1, "taker_fee_bps": 5, "tier": "base",
            "schedule": "standard", "volume_30d": "0", "volume_30d_estimated": false
        })))
        .mount(&server)
        .await;

    let fees = authed(server.uri()).fetch_account_fees().await.unwrap();
    assert!(fees.discounts.is_empty());
}

// --- Auth ------------------------------------------------------------------

#[tokio::test]
async fn portfolio_endpoints_require_credentials() {
    // All four are HMAC-gated; without credentials the SDK must refuse locally
    // rather than send an unauthenticated request.
    let server = MockServer::start().await;
    let anon = Client::new(Config::with_base_url(server.uri()));

    assert!(anon.fetch_account_summary().await.is_err());
    assert!(anon.fetch_account_state().await.is_err());
    assert!(anon.fetch_account_fees().await.is_err());
    assert!(anon.fetch_portfolio_history(None, None).await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}
