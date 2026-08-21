//! Isolated-margin adjustment, order amend (cancel-replace), batch order entry,
//! and client-assigned order ids. Covers wire (de)serialization, request
//! signing, path-segment encoding, and the client-side validation guards.

use nexus_exchange::types::{
    AmendOrder, Decimal, MarginDirection, OrderRequest, OrderResult, Side, TimeInForce,
};
use nexus_exchange::{Client, Config, Error};
use wiremock::matchers::{body_json, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

fn dec(s: &str) -> Decimal {
    s.parse::<Decimal>().unwrap()
}

#[tokio::test]
async fn adjust_margin_posts_signed_body_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/account/margin"))
        .and(header_exists("x-signature"))
        .and(body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP",
            "direction": "add",
            "amount": "100",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP",
            "allocated_margin": "350.00",
            "collateral": "9900.00",
        })))
        .mount(&server)
        .await;
    let r = authed(server.uri())
        .adjust_margin("BTC-USDX-PERP", MarginDirection::Add, dec("100"))
        .await
        .unwrap();
    assert_eq!(r.market_id, "BTC-USDX-PERP");
    assert_eq!(r.allocated_margin, dec("350.00"));
    assert_eq!(r.collateral, dec("9900.00"));
}

#[tokio::test]
async fn remove_margin_sends_remove_direction() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/account/margin"))
        .and(header_exists("x-signature"))
        .and(body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP",
            "direction": "remove",
            "amount": "25.5",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP",
            "allocated_margin": "324.50",
            "collateral": "9925.50",
        })))
        .mount(&server)
        .await;
    let r = authed(server.uri())
        .remove_margin("BTC-USDX-PERP", dec("25.5"))
        .await
        .unwrap();
    assert_eq!(r.allocated_margin, dec("324.50"));
}

#[tokio::test]
async fn adjust_margin_rejects_non_positive_amount_and_empty_market() {
    // No mock mounted: a request escaping the client would surface as a
    // transport error rather than the local validation error.
    let client = authed("http://127.0.0.1:1".to_string());
    let zero = client
        .add_margin("BTC-USDX-PERP", dec("0"))
        .await
        .unwrap_err();
    assert!(matches!(
        zero,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
    let empty = client
        .adjust_margin("", MarginDirection::Add, dec("100"))
        .await
        .unwrap_err();
    assert!(matches!(
        empty,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn amend_order_puts_only_changed_fields() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/orders/o1"))
        .and(wiremock::matchers::query_param(
            "market_id",
            "BTC-USDX-PERP",
        ))
        .and(header_exists("x-signature"))
        // Only `price` and `quantity` were set: the unset fields must be absent.
        .and(body_json(
            serde_json::json!({ "price": "50500", "quantity": "0.2" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "o2", "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Limit",
            "price": "50500", "quantity": "0.2", "time_in_force": "GTC", "status": "Open"
        })))
        .mount(&server)
        .await;
    let amend = AmendOrder::new().price(dec("50500")).quantity(dec("0.2"));
    let resp = authed(server.uri())
        .amend_order("o1", "BTC-USDX-PERP", &amend)
        .await
        .unwrap();
    assert_eq!(resp.id, "o2");
    assert_eq!(resp.price, Some(dec("50500")));
    assert_eq!(resp.quantity, dec("0.2"));
}

#[tokio::test]
async fn amend_order_serializes_tif_and_client_order_id() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/orders/o1"))
        .and(wiremock::matchers::query_param(
            "market_id",
            "BTC-USDX-PERP",
        ))
        .and(header_exists("x-signature"))
        // Exercises the `time_in_force` and `client_order_id` setters: TIF
        // serializes UPPERCASE, and only the two set fields appear in the body.
        .and(body_json(
            serde_json::json!({ "time_in_force": "IOC", "client_order_id": "replacement-1" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "o2", "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Limit",
            "time_in_force": "IOC", "status": "Open", "client_order_id": "replacement-1"
        })))
        .mount(&server)
        .await;
    let amend = AmendOrder::new()
        .time_in_force(TimeInForce::Ioc)
        .client_order_id("replacement-1");
    let resp = authed(server.uri())
        .amend_order("o1", "BTC-USDX-PERP", &amend)
        .await
        .unwrap();
    assert_eq!(resp.id, "o2");
    assert_eq!(resp.time_in_force, TimeInForce::Ioc);
    assert_eq!(resp.client_order_id.as_deref(), Some("replacement-1"));
}

#[tokio::test]
async fn amend_order_with_no_changes_is_rejected() {
    let err = authed("http://127.0.0.1:1".to_string())
        .amend_order("o1", "BTC-USDX-PERP", &AmendOrder::new())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn create_orders_posts_batch_and_parses_typed_results() {
    let server = MockServer::start().await;
    // The batch returns 201 with a per-order result array even when an entry was
    // rejected (sequential, non-atomic): one placed order plus one rejection, in
    // request order. Each entry is internally tagged by `outcome` (`ok`/`err`),
    // matching the engine's `BatchOrderResult`.
    Mock::given(method("POST"))
        .and(path("/api/v1/orders/batch"))
        .and(header_exists("x-signature"))
        .and(body_json(serde_json::json!([
            {
                "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Limit",
                "price": "50000", "quantity": "0.1", "time_in_force": "GTC"
            },
            {
                "market_id": "ETH-USDX-PERP", "side": "Sell", "order_type": "Market",
                "quantity": "999", "time_in_force": "IOC"
            }
        ])))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!([
            {
                "outcome": "ok",
                "order": {
                    "id": "o1", "market_id": "BTC-USDX-PERP", "side": "Buy",
                    "order_type": "Limit", "price": "50000", "quantity": "0.1",
                    "time_in_force": "GTC", "status": "Open"
                },
                "fills": []
            },
            {
                "outcome": "err",
                "error": "INSUFFICIENT_MARGIN",
                "message": "insufficient margin to place order"
            }
        ])))
        .mount(&server)
        .await;

    let orders = [
        OrderRequest::limit(
            "BTC-USDX-PERP",
            Side::Buy,
            dec("50000"),
            dec("0.1"),
            TimeInForce::Gtc,
        ),
        OrderRequest::market("ETH-USDX-PERP", Side::Sell, dec("999")),
    ];
    let results: Vec<OrderResult> = authed(server.uri()).create_orders(&orders).await.unwrap();

    assert_eq!(results.len(), 2);

    // Entry 0: placed — typed order record, no error.
    assert!(results[0].succeeded());
    assert!(results[0].error().is_none());
    let order = results[0].order().expect("placed entry exposes its order");
    assert_eq!(order.id, "o1");
    assert_eq!(order.price, Some(dec("50000")));
    assert!(matches!(&results[0], OrderResult::Placed { fills, .. } if fills.is_empty()));

    // Entry 1: rejected — typed (error, message), no order record.
    assert!(!results[1].succeeded());
    assert!(results[1].order().is_none());
    assert_eq!(
        results[1].error(),
        Some(("INSUFFICIENT_MARGIN", "insufficient margin to place order"))
    );
}

#[tokio::test]
async fn create_order_with_client_order_id_serializes_field() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .and(body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Market",
            "quantity": "0.1", "time_in_force": "IOC", "client_order_id": "my-id-1"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "o9", "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Market",
                "quantity": "0.1", "time_in_force": "IOC", "status": "Filled",
                "client_order_id": "my-id-1"
            },
            "fills": []
        })))
        .mount(&server)
        .await;
    let order = OrderRequest::market("BTC-USDX-PERP", Side::Buy, dec("0.1"))
        .with_client_order_id("my-id-1");
    let resp = authed(server.uri()).create_order(&order).await.unwrap();
    assert_eq!(resp.order.client_order_id.as_deref(), Some("my-id-1"));
}

#[tokio::test]
async fn empty_order_id_is_rejected_without_request() {
    // No mock is mounted: the path-segment guard must reject before any I/O, so
    // a request escaping the client would surface as a transport error instead.
    let err = authed("http://127.0.0.1:1".to_string())
        .amend_order("", "BTC-USDX-PERP", &AmendOrder::new().price(dec("100")))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}
