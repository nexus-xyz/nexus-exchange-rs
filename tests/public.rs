use nexus_exchange::rest::MAX_FUNDING_SAMPLES_LIMIT;
use nexus_exchange::{Client, Config};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
fn client(uri: String) -> Client {
    Client::new(Config::with_base_url(uri))
}

#[tokio::test]
async fn fetch_markets_parses_string_decimals() {
    let server = MockServer::start().await;
    let body = serde_json::json!([{
        "market_id": "BTC-USDX-PERP", "base_asset": "BTC", "quote_asset": "USDX",
        "tick_size": "0.1", "lot_size": "0.001", "min_order_size": "0.001",
        "max_order_size": "100", "initial_margin_rate": "0.05",
        "maintenance_margin_rate": "0.03", "max_leverage": 20
    }]);
    Mock::given(method("GET"))
        .and(path("/markets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let markets = client(server.uri()).fetch_markets().await.unwrap();
    assert_eq!(markets.len(), 1);
    assert_eq!(markets[0].market_id, "BTC-USDX-PERP");
    assert_eq!(markets[0].tick_size.to_string(), "0.1");
    assert_eq!(markets[0].max_leverage, 20);
}

#[tokio::test]
async fn fetch_ticker_parses_numbers_and_nulls() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "symbol": "BTC-USDX-PERP", "timestamp": 1776033900000i64, "datetime": "2026-04-13T00:00:00Z",
        "high": 51903.0, "low": 44992.0, "bid": null, "bidVolume": null, "ask": 50012.5,
        "askVolume": 1.2, "open": 48062.0, "close": 51903.0, "last": 51903.0, "change": 3841.0,
        "percentage": 7.99, "baseVolume": 27.1, "quoteVolume": 1350000.0,
        "markPrice": 50011.6, "indexPrice": 50010.0, "info": {}
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/ticker"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let t = client(server.uri())
        .fetch_ticker("BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(t.symbol, "BTC-USDX-PERP");
    assert_eq!(t.ask.unwrap().to_string(), "50012.5");
    assert!(t.bid.is_none());
}

#[tokio::test]
async fn error_envelope_is_decoded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/NOPE/ticker"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "code": "market_not_found", "message": "no such market"
        })))
        .mount(&server)
        .await;

    let err = client(server.uri()).fetch_ticker("NOPE").await.unwrap_err();
    // A 404 with an unmodeled code classifies as a terminal BadRequest that
    // still carries the server's machine-readable code + message.
    assert!(!err.is_retryable());
    match err {
        nexus_exchange::Error::Terminal(nexus_exchange::TerminalError::BadRequest {
            code,
            message,
        }) => {
            assert_eq!(code, "market_not_found");
            assert_eq!(message, "no such market");
        }
        other => panic!("expected Terminal(BadRequest), got {other:?}"),
    }
}

#[tokio::test]
async fn fetch_order_book_parses_number_levels() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "symbol": "BTC-USDX-PERP",
        "bids": [[50010.5, 1.2], [50010.0, 3.4]],
        "asks": [[50011.0, 0.5]],
        "timestamp": 1776033900000i64, "datetime": "2026-04-13T00:00:00Z", "nonce": 42
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/orderbook"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let ob = client(server.uri())
        .fetch_order_book("BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(ob.bids.len(), 2);
    assert_eq!(ob.bids[0].price().to_string(), "50010.5");
    assert_eq!(ob.bids[0].amount().to_string(), "1.2");
    assert_eq!(ob.asks[0].price().to_string(), "50011");
    assert_eq!(ob.nonce, 42);
}

#[tokio::test]
async fn fetch_trades_parses_side_and_limit() {
    use nexus_exchange::types::Side;
    let server = MockServer::start().await;
    let body = serde_json::json!([{
        "id": "t1", "symbol": "BTC-USDX-PERP", "price": 50010.5, "amount": 0.1, "cost": 5001.05,
        "side": "buy", "timestamp": 1776033900000i64, "datetime": "2026-04-13T00:00:00Z",
        "takerOrMaker": "taker", "is_liquidation": false, "info": {}
    }]);
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/trades"))
        .and(wiremock::matchers::query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let trades = client(server.uri())
        .fetch_trades("BTC-USDX-PERP", Some(1))
        .await
        .unwrap();
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].side, Side::Buy);
    assert_eq!(trades[0].taker_or_maker.as_deref(), Some("taker"));
}

#[tokio::test]
async fn fetch_funding_parses_string_decimals() {
    let server = MockServer::start().await;
    let body = serde_json::json!([{
        "timestamp": 1776033900000i64, "funding_rate": "0.0001", "premium_index": "0.00005",
        "mark_price": "50011.60", "oracle_price": "50010.00"
    }]);
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/funding"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let f = client(server.uri())
        .fetch_funding_rate_history("BTC-USDX-PERP", None)
        .await
        .unwrap();
    assert_eq!(f[0].funding_rate.to_string(), "0.0001");
    assert_eq!(f[0].mark_price.to_string(), "50011.60");
}

#[tokio::test]
async fn fetch_funding_premium_samples_parses_and_passes_limit() {
    let server = MockServer::start().await;
    let body = serde_json::json!([
        { "timestamp": 1776033900000i64, "premium_index": "0.00012" },
        { "timestamp": 1776033960000i64, "premium_index": "-0.00004" },
    ]);
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/funding-samples"))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let s = client(server.uri())
        .fetch_funding_premium_samples("BTC-USDX-PERP", Some(2))
        .await
        .unwrap();
    assert_eq!(s[0].timestamp, 1776033900000);
    // Decimal, not f64: a premium of 0.00012 read through a float would not
    // round-trip to this string.
    assert_eq!(s[0].premium_index.to_string(), "0.00012");
    // Signed — the perpetual can trade below the index.
    assert_eq!(s[1].premium_index.to_string(), "-0.00004");
}

#[tokio::test]
async fn fetch_funding_premium_samples_rejects_an_out_of_range_limit_before_sending() {
    // No mock is mounted: if the clamp did not fire, the request would go out
    // and fail as a connection error instead, so this also proves the check runs
    // before the send rather than after the response.
    let c = client("http://127.0.0.1:1".to_string());
    for limit in [0, MAX_FUNDING_SAMPLES_LIMIT + 1] {
        let err = c
            .fetch_funding_premium_samples("BTC-USDX-PERP", Some(limit))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                nexus_exchange::Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
            ),
            "limit {limit} must be rejected locally, got {err:?}"
        );
    }
    // And the boundary is inclusive — 480 is a legal ask, so it must NOT be
    // rejected locally (it fails on connect instead).
    let err = c
        .fetch_funding_premium_samples("BTC-USDX-PERP", Some(MAX_FUNDING_SAMPLES_LIMIT))
        .await
        .unwrap_err();
    assert!(
        !matches!(
            err,
            nexus_exchange::Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
        ),
        "the maximum itself must be accepted, got {err:?}"
    );
}

#[tokio::test]
async fn fetch_ohlcv_parses_array_candles() {
    let server = MockServer::start().await;
    let body = serde_json::json!([[1776033900000i64, 48062.0, 51903.0, 44992.0, 51903.0, 27.123]]);
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/candles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let c = client(server.uri())
        .fetch_ohlcv("BTC-USDX-PERP", Some("1m"), Some(1))
        .await
        .unwrap();
    assert_eq!(c[0].timestamp(), 1776033900000);
    assert_eq!(c[0].close().to_string(), "51903");
    assert_eq!(c[0].volume().to_string(), "27.123");
}
#[tokio::test]
async fn fetch_ticker_tolerates_omitted_fields() {
    // Forward-compat: a *missing* (not null) number field must default to None
    // rather than hard-error, so an older client keeps parsing a slimmer or
    // re-shaped ticker. Here `high`/`low`/`change`/`info` are omitted entirely.
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "symbol": "BTC-USDX-PERP", "timestamp": 1776033900000i64,
        "datetime": "2026-04-13T00:00:00Z", "bid": 50010.0, "ask": 50012.5,
        "last": 50011.6
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/ticker"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let t = client(server.uri())
        .fetch_ticker("BTC-USDX-PERP")
        .await
        .unwrap();
    assert!(t.high.is_none());
    assert!(t.low.is_none());
    assert!(t.change.is_none());
    assert_eq!(t.bid.unwrap().to_string(), "50010");
}

#[tokio::test]
async fn fetch_ticker_float_decimal_is_not_lossy_for_nice_values() {
    // Market-data money rides the f64 `float` adapter (the server sends these
    // as JSON numbers). Guard the boundary with a value that is NOT f64-exact
    // (1.1) so a regression that widened/changed the decode would surface here,
    // rather than only testing f64-exact literals.
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "symbol": "BTC-USDX-PERP", "timestamp": 1776033900000i64,
        "datetime": "2026-04-13T00:00:00Z", "last": 1.1, "percentage": 0.3, "info": {}
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/ticker"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let t = client(server.uri())
        .fetch_ticker("BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(t.last.unwrap().to_string(), "1.1");
    assert_eq!(t.percentage.unwrap().to_string(), "0.3");
}
#[tokio::test]
async fn fetch_market_summaries_parses_numbers_and_halted_null() {
    // /markets/summary -> [MarketSummary]. last_trade_price is `["number","null"]`:
    // a halted market sends null, so this exercises the Option<Decimal> path.
    let server = MockServer::start().await;
    let body = serde_json::json!([
        {
            "market_id": "BTC-USDX-PERP", "last_trade_price": 50011.6, "volume_24h": 1350000.0,
            "trade_count": 982, "status": "active", "halt_reason": null,
            "halted_at": null, "adl_event_count": 0
        },
        {
            "market_id": "DOGE-USDX-PERP", "last_trade_price": null, "volume_24h": 0.0,
            "trade_count": 0, "status": "halted", "halt_reason": "adl_pool_exhausted",
            "halted_at": 1776033900000i64, "adl_event_count": 3
        }
    ]);
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let s = client(server.uri()).fetch_market_summaries().await.unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].last_trade_price.unwrap().to_string(), "50011.6");
    assert_eq!(s[0].volume_24h.to_string(), "1350000");
    // halted market: null last_trade_price must not fail the whole decode.
    assert!(s[1].last_trade_price.is_none());
    assert_eq!(s[1].status, "halted");
    assert_eq!(s[1].halt_reason.as_deref(), Some("adl_pool_exhausted"));
    assert_eq!(s[1].halted_at, Some(1776033900000));
}

#[tokio::test]
async fn fetch_tickers_parses_market_keyed_map() {
    // /tickers -> bare object keyed by *market id* (spec: additionalProperties
    // Ticker, "Object keyed by market_id"). The spec carries no example for
    // this route, so this pins the schema-confirmed shape: a map, not a wrapped
    // envelope. Two markets verify that lookups key off the JSON object key.
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "BTC-USDX-PERP": {
            "symbol": "BTC-USDX-PERP", "timestamp": 1776033900000i64,
            "datetime": "2026-04-13T00:00:00Z", "high": 51903.0, "low": 44992.0,
            "bid": 50010.0, "bidVolume": 1.2, "ask": 50012.5, "askVolume": 1.2,
            "open": 48062.0, "close": 51903.0, "last": 51903.0, "change": 3841.0,
            "percentage": 7.99, "baseVolume": 27.1, "quoteVolume": 1350000.0,
            "markPrice": 50011.6, "indexPrice": 50010.0, "info": {}
        },
        "ETH-USDX-PERP": {
            "symbol": "ETH-USDX-PERP", "timestamp": 1776033900000i64,
            "datetime": "2026-04-13T00:00:00Z", "last": 3120.5, "markPrice": 3120.0
        }
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/tickers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let tickers = client(server.uri()).fetch_tickers().await.unwrap();
    assert_eq!(tickers.len(), 2);
    let t = tickers.get("BTC-USDX-PERP").expect("market id key present");
    assert_eq!(t.last.unwrap().to_string(), "51903");
    assert_eq!(t.mark_price.unwrap().to_string(), "50011.6");
    assert_eq!(
        tickers
            .get("ETH-USDX-PERP")
            .unwrap()
            .last
            .unwrap()
            .to_string(),
        "3120.5"
    );
}

#[tokio::test]
async fn fetch_tickers_empty_response_is_empty_map() {
    // The realistic "no data" shape is an empty object `{}` (the spec's example
    // is unfilled/null, which carries no shape). It must decode to an empty map
    // rather than erroring, so a caller can iterate over zero tickers safely.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tickers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let tickers = client(server.uri()).fetch_tickers().await.unwrap();
    assert!(tickers.is_empty());
}

#[tokio::test]
async fn fetch_mark_price_parses_string_decimal() {
    // /markets/{id}/mark-price. The spec documents this endpoint by example
    // only (no schema), so this pins the shape the SDK assumes: a string mark.
    let server = MockServer::start().await;
    let body = serde_json::json!({ "market_id": "BTC-USDX-PERP", "mark_price": "50011.60" });
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/mark-price"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let m = client(server.uri())
        .fetch_mark_price("BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(m.market_id, "BTC-USDX-PERP");
    assert_eq!(m.mark_price.to_string(), "50011.60");
}

#[tokio::test]
async fn fetch_market_status_parses_halt_fields() {
    // /markets/{id}/status -> MarketStatus.
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "market_id": "BTC-USDX-PERP", "status": "halted",
        "halt_reason": "adl_pool_exhausted", "halted_at": 1776033900000i64,
        "adl_event_count": 3
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/BTC-USDX-PERP/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let st = client(server.uri())
        .fetch_market_status("BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(st.status, "halted");
    assert_eq!(st.halt_reason.as_deref(), Some("adl_pool_exhausted"));
    assert_eq!(st.halted_at, Some(1776033900000));
    assert_eq!(st.adl_event_count, 3);
}

#[tokio::test]
async fn fetch_ticker_encodes_market_id_path_segment() {
    // A market id with characters that must be escaped to stay one path
    // segment (mirrors the account-path hardening in encoded_segment).
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "symbol": "a/b c", "timestamp": 1776033900000i64, "datetime": "2026-04-13T00:00:00Z",
        "high": 1.0, "low": 1.0, "bid": null, "bidVolume": null, "ask": null,
        "askVolume": null, "open": 1.0, "close": 1.0, "last": 1.0, "change": 0.0,
        "percentage": 0.0, "baseVolume": 0.0, "quoteVolume": 0.0,
        "markPrice": 1.0, "indexPrice": 1.0, "info": {}
    });
    Mock::given(method("GET"))
        .and(path("/api/v1/markets/a%2Fb%20c/ticker"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let t = client(server.uri()).fetch_ticker("a/b c").await.unwrap();
    assert_eq!(t.symbol, "a/b c");
}

#[tokio::test]
async fn public_market_reads_reject_empty_market_id_locally() {
    // An empty market id must be rejected before any I/O, so it can't collapse
    // `/markets/{id}/status` into the parent collection route. Points at an
    // unroutable address to prove no request is attempted.
    use nexus_exchange::Error;
    let c = client("http://127.0.0.1:1".to_string());
    for err in [
        c.fetch_ticker("").await.unwrap_err(),
        c.fetch_order_book("").await.unwrap_err(),
        c.fetch_trades("", None).await.unwrap_err(),
        c.fetch_ohlcv("", None, None).await.unwrap_err(),
        c.fetch_funding_rate_history("", None).await.unwrap_err(),
        c.fetch_funding_premium_samples("", None).await.unwrap_err(),
        c.fetch_mark_price("").await.unwrap_err(),
        c.fetch_market_status("").await.unwrap_err(),
    ] {
        assert!(matches!(
            err,
            Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
        ));
    }
}

// ── Venue statistics: GET /stats and GET /stats/history ─────────────────────
//
// Both start from a `Client` method against a mock server, so they fail if the
// endpoint is unreachable from the public API rather than merely mis-typed —
// the ENG-8084 lesson (a `Paginator` with a green unit suite for months while
// nothing on `Client` returned one).

fn stats_body() -> serde_json::Value {
    serde_json::json!({
        "events_received": 1_234_567i64,
        "fills_total": 89_012i64,
        "liquidations_total": 34i64,
        "gap_count": 0i64,
        "connected": true,
        "last_event_ms": 1_776_033_900_000i64,
        "uptime_seconds": 86_400i64,
        "events_per_sec": 42.5,
        "health": "Healthy",
        "highest_sequence_seen": 9_876_543i64,
        "unique_traders_24h": 512i64,
        "unique_traders_7d": 2_048i64,
        "unique_traders_30d": 8_192i64
    })
}

#[tokio::test]
async fn fetch_stats_reads_counters_and_trader_rollups() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(stats_body()))
        .mount(&server)
        .await;

    let stats = client(server.uri()).fetch_stats().await.unwrap();
    assert_eq!(stats.events_received, 1_234_567);
    assert_eq!(stats.fills_total, 89_012);
    assert_eq!(stats.liquidations_total, 34);
    assert_eq!(stats.gap_count, 0);
    assert!(stats.connected);
    assert_eq!(stats.last_event_ms, Some(1_776_033_900_000));
    assert_eq!(stats.uptime_seconds, 86_400);
    assert_eq!(stats.events_per_sec, 42.5);
    assert_eq!(stats.health, "Healthy");
    assert_eq!(stats.highest_sequence_seen, 9_876_543);
    assert_eq!(stats.unique_traders_24h, Some(512));
    assert_eq!(stats.unique_traders_7d, Some(2_048));
    assert_eq!(stats.unique_traders_30d, Some(8_192));
}

/// An absent trader rollup must decode as `None`, never as `Some(0)`.
///
/// The spec documents the three `unique_traders_*` fields as "Present on
/// `/stats`", i.e. an augmentation rather than part of the base snapshot. A
/// consumer rendering DAU has to tell "not reported" from "nobody traded"; a
/// defaulted `0` would render an outage as a real zero.
#[tokio::test]
async fn absent_trader_rollups_are_none_not_zero() {
    let server = MockServer::start().await;
    let mut body = stats_body();
    let obj = body.as_object_mut().unwrap();
    obj.remove("unique_traders_24h");
    obj.remove("unique_traders_7d");
    obj.remove("unique_traders_30d");
    Mock::given(method("GET"))
        .and(path("/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let stats = client(server.uri()).fetch_stats().await.unwrap();
    assert_eq!(stats.unique_traders_24h, None);
    assert_eq!(stats.unique_traders_7d, None);
    assert_eq!(stats.unique_traders_30d, None);
    // A reported zero is still a zero — the two cases must stay distinguishable.
    assert_eq!(stats.fills_total, 89_012);
}

/// `last_event_ms` is `None` before the first event, not the Unix epoch.
#[tokio::test]
async fn null_last_event_ms_is_none_not_epoch() {
    let server = MockServer::start().await;
    let mut body = stats_body();
    body.as_object_mut()
        .unwrap()
        .insert("last_event_ms".into(), serde_json::Value::Null);
    Mock::given(method("GET"))
        .and(path("/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let stats = client(server.uri()).fetch_stats().await.unwrap();
    assert_eq!(stats.last_event_ms, None);
}

#[tokio::test]
async fn fetch_stats_history_reads_the_throughput_ring() {
    let server = MockServer::start().await;
    let body = serde_json::json!([
        { "timestamp": 1_776_033_900i64, "fills": 12i64 },
        { "timestamp": 1_776_033_901i64, "fills": 0i64 },
        { "timestamp": 1_776_033_902i64, "fills": 7i64 }
    ]);
    Mock::given(method("GET"))
        .and(path("/stats/history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let hist = client(server.uri()).fetch_stats_history().await.unwrap();
    assert_eq!(hist.len(), 3);
    // Oldest-first, and seconds (not ms) — the one unit exception in this crate.
    assert_eq!(hist[0].timestamp, 1_776_033_900);
    assert_eq!(hist[0].fills, 12);
    // A genuine zero-fill second survives as 0 rather than being dropped.
    assert_eq!(hist[1].fills, 0);
    assert_eq!(hist[2].fills, 7);
}

/// An empty ring is a valid 200, not an error — a freshly started indexer has
/// not filled the buffer yet.
#[tokio::test]
async fn empty_stats_history_is_ok_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stats/history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let hist = client(server.uri()).fetch_stats_history().await.unwrap();
    assert!(hist.is_empty());
}

// ── GET /markets/{market_id}/risk-params ────────────────────────────────────

#[tokio::test]
async fn fetch_market_risk_params_parses_string_decimal_rates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/markets/BTC-USDX-PERP/risk-params"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP",
            "max_leverage": 20,
            "initial_margin_rate": "0.05",
            "maintenance_margin_rate": "0.03"
        })))
        .mount(&server)
        .await;

    let rp = client(server.uri())
        .fetch_market_risk_params("BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(rp.market_id, "BTC-USDX-PERP");
    assert_eq!(rp.max_leverage, 20);
    // Lossless decimals, not floats — a margin rate is money-adjacent.
    assert_eq!(rp.initial_margin_rate.to_string(), "0.05");
    assert_eq!(rp.maintenance_margin_rate.to_string(), "0.03");
    // Maintenance is the liquidation threshold, so it must be the lower of the two.
    assert!(rp.maintenance_margin_rate < rp.initial_margin_rate);
}

/// An unknown market is a `404` error, never an all-zero struct. A zeroed margin
/// rate would read as "no margin required", which is the most dangerous default
/// this payload could have.
#[tokio::test]
async fn unknown_market_risk_params_is_an_error_not_zeroed_rates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/markets/NOPE-PERP/risk-params"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(
                serde_json::json!({ "code": "NOT_FOUND", "message": "unknown market" }),
            ),
        )
        .mount(&server)
        .await;

    assert!(client(server.uri())
        .fetch_market_risk_params("NOPE-PERP")
        .await
        .is_err());
}

/// An empty market id is rejected locally, before any I/O — same contract as the
/// other single-market reads (see
/// `public_market_reads_reject_empty_market_id_locally`).
///
/// Note the guard is `is_empty()`, not `trim().is_empty()`: a whitespace-only id
/// is percent-encoded and sent, and the server answers 404. That asymmetry with
/// `require_non_empty` (which does reject blanks) is pre-existing and not
/// widened here.
#[tokio::test]
async fn risk_params_rejects_an_empty_market_id_locally() {
    let server = MockServer::start().await;
    assert!(client(server.uri())
        .fetch_market_risk_params("")
        .await
        .is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}
