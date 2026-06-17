use nexus_exchange::types::{Decimal, OrderRequest, Side, TimeInForce};
use nexus_exchange::{Client, Config};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

#[tokio::test]
async fn create_order_serializes_pascalcase_and_parses_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(header("x-api-key", "nx_test"))
        // proves enum serialization (Buy/Limit/GTC) and decimal-string fields
        .and(body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Limit",
            "price": "50000", "quantity": "0.1", "time_in_force": "GTC"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "o1", "market_id": "BTC-USDX-PERP", "account_id": "0xabc", "side": "Buy",
                "order_type": "Limit", "price": "50000", "quantity": "0.1", "filled_qty": "0",
                "status": "Open", "time_in_force": "GTC", "created_at": 1, "updated_at": 1
            },
            "fills": []
        })))
        .mount(&server)
        .await;

    let order = OrderRequest::limit(
        "BTC-USDX-PERP",
        Side::Buy,
        "50000".parse::<Decimal>().unwrap(),
        "0.1".parse::<Decimal>().unwrap(),
        TimeInForce::Gtc,
    );
    let resp = authed(server.uri()).create_order(&order).await.unwrap();
    assert_eq!(resp.order.id, "o1");
    assert_eq!(resp.order.status, "Open");
    assert_eq!(resp.order.side, Side::Buy);
}

#[tokio::test]
async fn fetch_open_orders_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "id": "o1", "market_id": "BTC-USDX-PERP", "account_id": "0xabc", "side": "Sell",
            "order_type": "Limit", "price": "51000", "quantity": "0.2", "filled_qty": "0.05",
            "status": "PartiallyFilled", "time_in_force": "GTC", "created_at": 1, "updated_at": 2
        }])))
        .mount(&server)
        .await;
    let orders = authed(server.uri()).fetch_open_orders().await.unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].filled_qty.to_string(), "0.05");
}

#[tokio::test]
async fn cancel_order_returns_ack() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/orders/o1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "Cancelled"})),
        )
        .mount(&server)
        .await;
    let ack = authed(server.uri()).cancel_order("o1").await.unwrap();
    assert_eq!(ack["status"], "Cancelled");
}
