use nexus_exchange::{Client, Config};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        .and(path("/markets/BTC-USDX-PERP/ticker"))
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
        .and(path("/markets/NOPE/ticker"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "code": "market_not_found", "message": "no such market"
        })))
        .mount(&server)
        .await;

    let err = client(server.uri()).fetch_ticker("NOPE").await.unwrap_err();
    match err {
        nexus_exchange::Error::Api { code, message } => {
            assert_eq!(code, "market_not_found");
            assert_eq!(message, "no such market");
        }
        other => panic!("expected Api error, got {other:?}"),
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
        .and(path("/markets/BTC-USDX-PERP/orderbook"))
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
        .and(path("/markets/BTC-USDX-PERP/trades"))
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
        .and(path("/markets/BTC-USDX-PERP/funding"))
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
async fn fetch_ohlcv_parses_array_candles() {
    let server = MockServer::start().await;
    let body = serde_json::json!([[1776033900000i64, 48062.0, 51903.0, 44992.0, 51903.0, 27.123]]);
    Mock::given(method("GET"))
        .and(path("/markets/BTC-USDX-PERP/candles"))
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
