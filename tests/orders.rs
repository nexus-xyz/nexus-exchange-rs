use nexus_exchange::types::{Decimal, OrderRequest, SelfTradePrevention, Side, TimeInForce};
use nexus_exchange::{Client, Config, Error};
use wiremock::matchers::{
    body_json, body_string, header, header_exists, method, path, query_param,
    query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
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
        .and(path("/api/v1/orders"))
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
        .and(path("/api/v1/orders"))
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
        .and(path("/api/v1/orders/o1"))
        .and(wiremock::matchers::query_param(
            "market_id",
            "BTC-USDX-PERP",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "Cancelled"})),
        )
        .mount(&server)
        .await;
    let ack = authed(server.uri())
        .cancel_order("o1", "BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(ack["status"], "Cancelled");
}

#[tokio::test]
async fn cancel_all_orders_sends_no_market_filter() {
    // Account-wide cancel must hit DELETE /orders with no body and, crucially,
    // no `market_id` query param — otherwise it would scope to a market.
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/orders"))
        .and(query_param_is_missing("market_id"))
        .and(body_string(""))
        .and(header_exists("x-signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "cancelled": 7 })),
        )
        .mount(&server)
        .await;
    let ack = authed(server.uri()).cancel_all_orders().await.unwrap();
    assert_eq!(ack["cancelled"], 7);
}

#[tokio::test]
async fn cancel_orders_for_market_scopes_to_market() {
    // Market-scoped cancel hits the same DELETE /orders route but carries the
    // `market_id` query param, and the request stays signed (x-signature) over
    // the path+query that is actually sent.
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/orders"))
        .and(query_param("market_id", "BTC-USDX-PERP"))
        .and(body_string(""))
        .and(header_exists("x-signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "cancelled": 3 })),
        )
        .mount(&server)
        .await;
    let ack = authed(server.uri())
        .cancel_orders_for_market("BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(ack["cancelled"], 3);
}

#[tokio::test]
async fn cancel_orders_for_market_rejects_blank_market() {
    // A blank market — empty or whitespace-only — must be rejected locally (no
    // request sent) so it can never silently widen into an account-wide flatten
    // via the bare DELETE /orders. The unroutable host proves rejection is local.
    for blank in ["", "   "] {
        let err = authed("http://127.0.0.1:1".to_string())
            .cancel_orders_for_market(blank)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            Error::Terminal(nexus_exchange::TerminalError::InvalidRequest(_))
        ));
    }
}

// --- POST /api/v1/orders/preview (ENG-7928) ---------------------------------

/// The preview route. Kept as a constant so a test can assert the request went
/// *here* and nowhere near the placement route.
const PREVIEW_PATH: &str = "/api/v1/orders/preview";

fn preview_order_request() -> OrderRequest {
    OrderRequest::limit(
        "BTC-USDX-PERP",
        Side::Buy,
        "50000".parse::<Decimal>().unwrap(),
        "0.1".parse::<Decimal>().unwrap(),
        TimeInForce::Gtc,
    )
}

#[tokio::test]
async fn preview_order_signs_the_preview_route_and_parses_every_projection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .and(header("x-api-key", "nx_test"))
        .and(header_exists("x-signature"))
        // Same body shape as `create_order`: enums PascalCase/UPPERCASE, money as
        // decimal strings.
        .and(body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Limit",
            "price": "50000", "quantity": "0.1", "time_in_force": "GTC"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accepted": true,
            "reject_reason": null,
            // More significant digits than an f64 can hold: if any of these went
            // through a float adapter the round-trip below would not be exact.
            "required_initial_margin": "1666.666666666666666666666666",
            "projected_post_trade_equity": "98333.33333333333333333333333",
            "projected_post_trade_liquidation_price": "33333.123456789012345678901",
            // `Decimal` in the spec = a decimal STRING, even though the
            // `leverage` request parameter elsewhere in the API is a JSON number.
            "projected_post_trade_leverage": "3.0000000000000000000000001",
            "expected_fill_vwap": "50000.00000000000000000000001",
            "projected_fees": "1.5000000000000000000000001"
        })))
        .mount(&server)
        .await;

    let preview = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .unwrap();

    assert!(preview.is_accepted());
    assert_eq!(preview.accepted, Some(true));
    assert_eq!(preview.reject_reason, None);
    // Exact decimal-string round-trips — no float rounding artifacts anywhere.
    for (got, want) in [
        (
            preview.required_initial_margin,
            "1666.666666666666666666666666",
        ),
        (
            preview.projected_post_trade_equity,
            "98333.33333333333333333333333",
        ),
        (
            preview.projected_post_trade_liquidation_price,
            "33333.123456789012345678901",
        ),
        (
            preview.projected_post_trade_leverage,
            "3.0000000000000000000000001",
        ),
        (preview.expected_fill_vwap, "50000.00000000000000000000001"),
        (preview.projected_fees, "1.5000000000000000000000001"),
    ] {
        assert_eq!(got.unwrap().to_string(), want);
    }

    // A preview must not place anything: exactly one request, and it went to the
    // preview route rather than the placement route.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), PREVIEW_PATH);
}

#[tokio::test]
async fn preview_order_reports_a_rejection_as_ok_not_err() {
    // A projection that the order WOULD be rejected is the endpoint answering its
    // question — a 200, not an `Err`. Mapping it to an error would make the
    // caller's `?` swallow the reason and hide it behind a transport-shaped
    // failure. The nulls also exercise the nullable projections: a rejected order
    // has no expected fill and no post-trade liquidation price.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accepted": false,
            "reject_reason": "InsufficientMargin",
            "required_initial_margin": "1666.67",
            "projected_post_trade_equity": "0",
            "projected_post_trade_liquidation_price": null,
            "projected_post_trade_leverage": "0",
            "expected_fill_vwap": null,
            "projected_fees": "0"
        })))
        .mount(&server)
        .await;

    let preview = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .expect("a rejected preview is a successful response");

    assert!(!preview.is_accepted());
    assert_eq!(preview.accepted, Some(false));
    assert_eq!(preview.reject_reason.as_deref(), Some("InsufficientMargin"));
    assert_eq!(preview.projected_post_trade_liquidation_price, None);
    assert_eq!(preview.expected_fill_vwap, None);
    // A reported zero is still a report — distinct from an absent field.
    assert_eq!(preview.projected_post_trade_equity.unwrap(), Decimal::ZERO);
}

#[tokio::test]
async fn preview_order_decodes_when_fields_are_absent_and_fails_closed() {
    // The spec gives `PreviewResponse` no `required` array, so the server may omit
    // any property. One absent field must not fail the whole decode — and an
    // absent `accepted` must never read as accepted.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let preview = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .expect("an all-absent preview must still decode");

    assert_eq!(preview.accepted, None);
    assert!(
        !preview.is_accepted(),
        "an unreported `accepted` must fail closed, not gate an order through"
    );
    assert_eq!(preview.reject_reason, None);
    assert_eq!(preview.required_initial_margin, None);
    assert_eq!(preview.projected_post_trade_equity, None);
    assert_eq!(preview.projected_post_trade_liquidation_price, None);
    assert_eq!(preview.projected_post_trade_leverage, None);
    assert_eq!(preview.expected_fill_vwap, None);
    assert_eq!(preview.projected_fees, None);
}

#[tokio::test]
async fn preview_order_forward_compatibly_ignores_unknown_fields() {
    // An additive spec field must not break the response, so the decode ignores
    // properties the SDK does not model yet.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accepted": true,
            "projected_fees": "1.5",
            "projected_slippage_bps": 7,
            "some_future_object": { "nested": ["also", "fine"] }
        })))
        .mount(&server)
        .await;

    let preview = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .expect("an unmodeled additive field must not fail the decode");
    assert!(preview.is_accepted());
    assert_eq!(preview.projected_fees.unwrap().to_string(), "1.5");
}

#[tokio::test]
async fn preview_order_surfaces_machine_readable_error_codes() {
    // Genuine request failures classify like every other endpoint, and each keeps
    // the server's `code` — a caller that only sees "bad request" with nothing
    // after the colon can't branch on what actually went wrong. `POST` is not
    // auto-retried, so one mocked response per case is the whole exchange.
    use nexus_exchange::{TerminalError, TransientError};

    // 400 with an engine order-parameter code → InvalidOrder.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            serde_json::json!({ "code": "InvalidTickSize", "message": "price off tick" }),
        ))
        .mount(&server)
        .await;
    let err = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("InvalidTickSize"));
    assert!(matches!(
        err,
        Error::Terminal(TerminalError::InvalidOrder { .. })
    ));
    assert!(!err.is_retryable());

    // 400 with a request-shape code → BadRequest, code preserved.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            serde_json::json!({ "code": "InvalidBody", "message": "limit order needs a price" }),
        ))
        .mount(&server)
        .await;
    let err = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("InvalidBody"));
    assert!(matches!(
        err,
        Error::Terminal(TerminalError::BadRequest { .. })
    ));

    // 401 → Auth, carrying the opaque `unauthorized` code the spec documents.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(serde_json::json!({ "code": "unauthorized" })),
        )
        .mount(&server)
        .await;
    let err = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("unauthorized"));
    assert!(matches!(err, Error::Terminal(TerminalError::Auth { .. })));

    // 429 → transient RateLimited, honoring Retry-After.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "2")
                .set_body_json(serde_json::json!({ "code": "RateLimitExceeded" })),
        )
        .mount(&server)
        .await;
    let err = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .unwrap_err();
    assert!(err.is_retryable());
    assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(2)));

    // 5xx → transient Unavailable that still carries status and code.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PREVIEW_PATH))
        .respond_with(
            ResponseTemplate::new(502)
                .set_body_json(serde_json::json!({ "code": "authoritative_margin_unavailable" })),
        )
        .mount(&server)
        .await;
    let err = authed(server.uri())
        .preview_order(&preview_order_request())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("authoritative_margin_unavailable"));
    assert!(err.is_retryable());
    let rendered = err.to_string();
    assert!(
        rendered.contains("502") && rendered.contains("authoritative_margin_unavailable"),
        "5xx must not render as an empty code: {rendered}"
    );
    assert!(matches!(
        err,
        Error::Transient(TransientError::Unavailable { status: 502, .. })
    ));
}

// --- stp / max_slippage_bps / cancellation_reason (ENG-13068) ---------------

/// A minimal `Order` JSON body, so a test can vary one field without restating
/// the required ones.
fn order_json(extra: serde_json::Value) -> serde_json::Value {
    let mut base = serde_json::json!({
        "id": "o1", "market_id": "BTC-USDX-PERP", "account_id": "0xabc", "side": "Buy",
        "order_type": "Limit", "price": "50000", "quantity": "0.1", "filled_qty": "0",
        "status": "Cancelled", "time_in_force": "GTC", "created_at": 1, "updated_at": 2
    });
    let (serde_json::Value::Object(base_map), serde_json::Value::Object(extra_map)) =
        (&mut base, extra)
    else {
        panic!("order_json takes an object");
    };
    base_map.extend(extra_map);
    base
}

/// Fetch one order carrying `extra`, and hand it back for assertions.
async fn order_with(extra: serde_json::Value) -> nexus_exchange::types::Order {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orders"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([order_json(extra)])),
        )
        .mount(&server)
        .await;
    authed(server.uri())
        .fetch_open_orders()
        .await
        .unwrap()
        .pop()
        .expect("one order")
}

#[tokio::test]
async fn order_request_serializes_stp_and_max_slippage_bps_only_when_set() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        // Exact body: the two new fields appear with their PascalCase /
        // integer wire forms, and nothing else was added to the payload.
        .and(body_json(serde_json::json!({
            "market_id": "BTC-USDX-PERP", "side": "Buy", "order_type": "Market",
            "quantity": "0.1", "time_in_force": "IOC",
            "stp": "DecrementAndCancel", "max_slippage_bps": 50
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": order_json(serde_json::json!({"status": "Open"})),
            "fills": []
        })))
        .mount(&server)
        .await;

    let order = OrderRequest::market("BTC-USDX-PERP", Side::Buy, "0.1".parse().unwrap())
        .with_stp(SelfTradePrevention::DecrementAndCancel)
        .with_max_slippage_bps(50);
    authed(server.uri()).create_order(&order).await.unwrap();
}

#[test]
fn unset_stp_and_slippage_are_absent_from_the_payload() {
    // `None` must OMIT the keys, not send `null`: an explicit null would be a
    // different request, and the venue default (self-matching allowed, no cap)
    // has to stay the default for every existing caller.
    let body = serde_json::to_value(OrderRequest::market(
        "BTC-USDX-PERP",
        Side::Buy,
        "0.1".parse().unwrap(),
    ))
    .unwrap();
    assert!(
        body.get("stp").is_none(),
        "stp leaked into the payload: {body}"
    );
    assert!(
        body.get("max_slippage_bps").is_none(),
        "max_slippage_bps leaked into the payload: {body}"
    );
}

#[test]
fn zero_slippage_cap_is_sent_not_swallowed() {
    // `Some(0)` is a real, meaningful cap (band collapsed onto the mid), not an
    // "unset" sentinel — it must reach the wire.
    let body = serde_json::to_value(
        OrderRequest::market("BTC-USDX-PERP", Side::Buy, "0.1".parse().unwrap())
            .with_max_slippage_bps(0),
    )
    .unwrap();
    assert_eq!(body["max_slippage_bps"], serde_json::json!(0));
}

#[tokio::test]
async fn order_parses_echoed_stp_and_max_slippage_bps() {
    let order = order_with(serde_json::json!({
        "stp": "CancelOldest", "max_slippage_bps": 25
    }))
    .await;
    assert_eq!(order.stp.as_deref(), Some("CancelOldest"));
    assert_eq!(order.max_slippage_bps, Some(25));
}

#[tokio::test]
async fn order_without_stp_or_cap_decodes_to_none() {
    // Absent and explicit-null must both mean "placed without one", never a
    // fabricated default.
    let absent = order_with(serde_json::json!({})).await;
    assert_eq!(absent.stp, None);
    assert_eq!(absent.max_slippage_bps, None);
    assert_eq!(absent.cancellation_reason, None);

    let nulled = order_with(serde_json::json!({
        "stp": null, "max_slippage_bps": null, "cancellation_reason": null
    }))
    .await;
    assert_eq!(nulled.stp, None);
    assert_eq!(nulled.max_slippage_bps, None);
    assert_eq!(nulled.cancellation_reason, None);
}

#[tokio::test]
async fn cancellation_reason_parses_the_bare_string_form() {
    let order = order_with(serde_json::json!({"cancellation_reason": "SlippageCap"})).await;
    let reason = order.cancellation_reason.expect("a reason");
    assert_eq!(reason.cause(), Some("SlippageCap"));
    assert_eq!(reason.payload(), None);
    assert_eq!(reason.stp_mode(), None);
}

#[tokio::test]
async fn cancellation_reason_parses_the_stp_object_form() {
    // The shape a plain `Option<String>` would have failed to decode, taking the
    // whole Order with it.
    let order =
        order_with(serde_json::json!({"cancellation_reason": {"Stp": "CancelNewest"}})).await;
    let reason = order.cancellation_reason.expect("a reason");
    assert_eq!(reason.cause(), Some("Stp"));
    assert_eq!(reason.stp_mode(), Some("CancelNewest"));
}

#[tokio::test]
async fn an_unknown_cause_still_decodes_verbatim() {
    // The cause set is open in both shapes. A cause added engine-side must
    // surface, not fail the decode — a client on an older spec tag will meet one.
    let bare = order_with(serde_json::json!({"cancellation_reason": "SomeFutureCause"})).await;
    assert_eq!(
        bare.cancellation_reason
            .and_then(|r| r.cause().map(str::to_owned)),
        Some("SomeFutureCause".into())
    );

    let tagged =
        order_with(serde_json::json!({"cancellation_reason": {"FutureCause": {"detail": 7}}}))
            .await;
    let reason = tagged.cancellation_reason.expect("a reason");
    assert_eq!(reason.cause(), Some("FutureCause"));
    assert_eq!(reason.payload(), Some(&serde_json::json!({"detail": 7})));
    // Not the Stp cause, so no mode — even though it is the object form.
    assert_eq!(reason.stp_mode(), None);
}

#[tokio::test]
async fn an_unexpected_reason_shape_does_not_break_the_order_decode() {
    // Defence in depth: a shape neither documented form covers lands in `Other`
    // rather than failing the enclosing Order. A diagnostic field must never
    // cost the caller their order.
    let order = order_with(serde_json::json!({"cancellation_reason": [1, 2]})).await;
    assert_eq!(order.id, "o1");
    let reason = order.cancellation_reason.expect("a reason");
    assert_eq!(reason.cause(), None);
    assert_eq!(reason.payload(), Some(&serde_json::json!([1, 2])));
}
