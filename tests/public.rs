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
        nexus_exchange::Error::Api {
            status,
            code,
            message,
        } => {
            assert_eq!(status, 404);
            assert_eq!(code, "market_not_found");
            assert_eq!(message, "no such market");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}
