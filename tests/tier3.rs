//! Tier 3 endpoints: leverage / margin setters, order amend (cancel-replace),
//! batch cancel, client-order-id lookup/cancel, funding-payment & transfer
//! history, and sub-accounts. Covers wire (de)serialization, request signing,
//! path-segment encoding, and the client-side validation guards.

use nexus_exchange::types::{
    AmendOrder, Decimal, MarginMode, OrderRequest, OrderResult, Side, TimeInForce, TransferRequest,
};
use nexus_exchange::{Client, Config, Error};
use wiremock::matchers::{body_json, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
async fn set_leverage_posts_body_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/account/leverage"))
        .and(header_exists("x-signature"))
        .and(body_json(
            serde_json::json!({ "market_id": "BTC-USDX-PERP", "leverage": 10 }),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "market_id": "BTC-USDX-PERP", "leverage": 10 })),
        )
        .mount(&server)
        .await;
    let r = authed(server.uri())
        .set_leverage("BTC-USDX-PERP", 10)
        .await
        .unwrap();
    assert_eq!(r.leverage, 10);
    assert_eq!(r.market_id, "BTC-USDX-PERP");
}

#[tokio::test]
async fn set_leverage_zero_is_rejected_without_request() {
    // No mock mounted: if a request escaped the client the test would fail with
    // a connection/HTTP error rather than the local validation error.
    let err = authed("http://127.0.0.1:1".to_string())
        .set_leverage("BTC-USDX-PERP", 0)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn set_margin_mode_serializes_lowercase() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/account/margin-mode"))
        .and(body_json(
            serde_json::json!({ "market_id": "BTC-USDX-PERP", "margin_mode": "isolated" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "market_id": "BTC-USDX-PERP", "margin_mode": "isolated" }),
        ))
        .mount(&server)
        .await;
    let r = authed(server.uri())
        .set_margin_mode("BTC-USDX-PERP", MarginMode::Isolated)
        .await
        .unwrap();
    assert_eq!(r.margin_mode, MarginMode::Isolated);
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
            "order": {
                "id": "o1", "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Limit",
                "price": "50500", "quantity": "0.2", "time_in_force": "GTC", "status": "Open"
            },
            "fills": []
        })))
        .mount(&server)
        .await;
    let amend = AmendOrder::new().price(dec("50500")).quantity(dec("0.2"));
    let resp = authed(server.uri())
        .amend_order("o1", "BTC-USDX-PERP", &amend)
        .await
        .unwrap();
    assert_eq!(resp.order.price, Some(dec("50500")));
    assert_eq!(resp.order.quantity, dec("0.2"));
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
            "order": {
                "id": "o1", "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Limit",
                "time_in_force": "IOC", "status": "Open", "client_order_id": "replacement-1"
            },
            "fills": []
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
    assert_eq!(resp.order.time_in_force, TimeInForce::Ioc);
    assert_eq!(resp.order.client_order_id.as_deref(), Some("replacement-1"));
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
async fn cancel_orders_posts_batch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders/batch-cancel"))
        .and(body_json(
            serde_json::json!({ "order_ids": ["o1", "o2", "o3"] }),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "cancelled": 3 })),
        )
        .mount(&server)
        .await;
    let ack = authed(server.uri())
        .cancel_orders(&["o1", "o2", "o3"])
        .await
        .unwrap();
    assert_eq!(ack["cancelled"], 3);
}

#[tokio::test]
async fn cancel_orders_empty_is_rejected() {
    let err = authed("http://127.0.0.1:1".to_string())
        .cancel_orders(&[])
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
        .and(path("/orders/batch"))
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
        .and(path("/orders"))
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
async fn fetch_order_by_client_id_hits_encoded_path() {
    let server = MockServer::start().await;
    // A client id with characters that must be escaped to stay one path segment.
    Mock::given(method("GET"))
        .and(path("/orders/by-client-id/a%2Fb%20c"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "o1", "market_id": "BTC-USDX-PERP", "side": "Buy",
            "order_type": "Limit", "time_in_force": "GTC", "client_order_id": "a/b c"
        })))
        .mount(&server)
        .await;
    let o = authed(server.uri())
        .fetch_order_by_client_id("a/b c")
        .await
        .unwrap();
    assert_eq!(o.client_order_id.as_deref(), Some("a/b c"));
}

#[tokio::test]
async fn cancel_order_by_client_id_deletes_encoded_path() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/orders/by-client-id/my-id-1"))
        .and(header_exists("x-signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "status": "Cancelled" })),
        )
        .mount(&server)
        .await;
    let ack = authed(server.uri())
        .cancel_order_by_client_id("my-id-1")
        .await
        .unwrap();
    assert_eq!(ack["status"], "Cancelled");
}

#[tokio::test]
async fn empty_client_order_id_is_rejected_without_request() {
    let err = authed("http://127.0.0.1:1".to_string())
        .fetch_order_by_client_id("")
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn fetch_funding_payments_filters_by_market() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/funding-payments"))
        .and(wiremock::matchers::query_param(
            "market_id",
            "BTC-USDX-PERP",
        ))
        .and(header_exists("x-signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "market_id": "BTC-USDX-PERP", "amount": "-1.25",
                "funding_rate": "0.0001", "timestamp": 1776033900000i64
            }])),
        )
        .mount(&server)
        .await;
    let p = authed(server.uri())
        .fetch_funding_payments(Some("BTC-USDX-PERP"))
        .await
        .unwrap();
    assert_eq!(p[0].amount, dec("-1.25"));
    assert_eq!(p[0].funding_rate, Some(dec("0.0001")));
}

#[tokio::test]
async fn create_transfer_sends_body_and_rejects_non_positive() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/transfers"))
        .and(body_json(serde_json::json!({
            "from_account": "main", "to_account": "sub1", "amount": "100"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "t1", "from_account": "main", "to_account": "sub1",
            "amount": "100", "timestamp": 1776033900000i64, "status": "completed"
        })))
        .mount(&server)
        .await;
    let req = TransferRequest::new("main", "sub1", dec("100"));
    let t = authed(server.uri()).create_transfer(&req).await.unwrap();
    assert_eq!(t.id, "t1");
    assert_eq!(t.status, "completed");

    // A zero/negative amount never leaves the client.
    let bad = TransferRequest::new("main", "sub1", dec("0"));
    let err = authed(server.uri())
        .create_transfer(&bad)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn fetch_transfers_lists_history() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/transfers"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "t1", "from_account": "main", "to_account": "sub1",
                "amount": "100", "timestamp": 1776033900000i64, "status": "completed"
            },
            // Slim record: a missing `status` defaults rather than failing decode.
            { "id": "t2", "from_account": "sub1", "to_account": "main",
              "amount": "25.5", "timestamp": 1776034000000i64 }
        ])))
        .mount(&server)
        .await;
    let transfers = authed(server.uri()).fetch_transfers().await.unwrap();
    assert_eq!(transfers.len(), 2);
    assert_eq!(transfers[0].id, "t1");
    assert_eq!(transfers[0].amount, dec("100"));
    assert_eq!(transfers[1].status, "");
}

#[tokio::test]
async fn leverage_and_margin_mode_reject_empty_market_id() {
    // The market id rides in the JSON body, so it isn't covered by the path
    // segment guard — these assert the explicit body-side check. No mock is
    // mounted: a request escaping the client would surface as a transport error.
    let client = authed("http://127.0.0.1:1".to_string());
    let lev = client.set_leverage("", 10).await.unwrap_err();
    assert!(matches!(
        lev,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
    let margin = client
        .set_margin_mode("", MarginMode::Cross)
        .await
        .unwrap_err();
    assert!(matches!(
        margin,
        Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn fetch_and_create_sub_accounts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sub-accounts"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "account_id": "sub1", "label": "desk-a", "equity": "1000.50" },
            { "account_id": "sub2" }
        ])))
        .mount(&server)
        .await;
    let subs = authed(server.uri()).fetch_sub_accounts().await.unwrap();
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].equity, Some(dec("1000.50")));
    // Slim payload: missing label/equity default rather than failing decode.
    assert_eq!(subs[1].label, "");
    assert_eq!(subs[1].equity, None);

    let server2 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sub-accounts"))
        .and(body_json(serde_json::json!({ "label": "desk-b" })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "account_id": "sub3", "label": "desk-b" })),
        )
        .mount(&server2)
        .await;
    let created = authed(server2.uri())
        .create_sub_account("desk-b")
        .await
        .unwrap();
    assert_eq!(created.account_id, "sub3");
}
