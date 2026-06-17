use nexus_exchange::{Client, Config};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

#[tokio::test]
async fn fetch_balance_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/account"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "balance": "1000.00", "collateral": "1000.00", "equity": "1012.34",
            "available_margin": "812.34",
            "positions": [{
                "market_id": "BTC-USDX-PERP", "side": "long", "size": "0.5",
                "entry_price": "50000", "unrealized_pnl": "12.34", "realized_pnl": "0",
                "liquidation_price": "40000"
            }]
        })))
        .mount(&server)
        .await;
    let acct = authed(server.uri()).fetch_balance().await.unwrap();
    assert_eq!(acct.equity.to_string(), "1012.34");
    assert_eq!(acct.positions.len(), 1);
    assert_eq!(acct.positions[0].market_id, "BTC-USDX-PERP");
}

#[tokio::test]
async fn fetch_my_trades_parses_fills() {
    use nexus_exchange::types::Side;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/fills"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": "f1", "order_id": "o1", "market_id": "BTC-USDX-PERP", "side": "sell",
                "price": "50010.5", "size": "0.1", "fee": "0.25", "taker_or_maker": "maker",
                "timestamp": 1776033900000i64, "is_liquidation": false
            }])),
        )
        .mount(&server)
        .await;
    let fills = authed(server.uri()).fetch_my_trades().await.unwrap();
    assert_eq!(fills[0].side, Side::Sell);
    assert_eq!(fills[0].fee.to_string(), "0.25");
}

#[tokio::test]
async fn fetch_rate_limit_status_handles_nulls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/account/rate-limit"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tier": "unlimited", "limit": null, "remaining": null, "reset_at_ms": null
        })))
        .mount(&server)
        .await;
    let rl = authed(server.uri())
        .fetch_rate_limit_status()
        .await
        .unwrap();
    assert_eq!(rl.tier, "unlimited");
    assert!(rl.limit.is_none());
}

#[tokio::test]
async fn fetch_balance_tolerates_missing_liquidation_price() {
    // liquidation_price isn't `required` in the spec; a position that omits it
    // must decode to None, not fail the whole fetch_balance call.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/account"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "balance": "1000.00", "collateral": "1000.00", "equity": "1000.00",
            "available_margin": "1000.00",
            "positions": [{
                "market_id": "ETH-USDX-PERP", "side": "long", "size": "1",
                "entry_price": "3000", "unrealized_pnl": "0", "realized_pnl": "0"
            }]
        })))
        .mount(&server)
        .await;
    let acct = authed(server.uri()).fetch_balance().await.unwrap();
    assert!(acct.positions[0].liquidation_price.is_none());
}
