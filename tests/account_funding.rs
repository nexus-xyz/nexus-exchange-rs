//! `GET /funding` — funding settlements for the authenticated account.
//!
//! Distinct from `GET /funding-payments` (`fetch_funding_payments`), which is a
//! narrower row. Every test starts from a `Client` method against a mock server
//! so it fails on unreachability, not only on a wrong type.

use nexus_exchange::rest::MAX_ACCOUNT_FUNDING_LIMIT;
use nexus_exchange::types::{Decimal, FundingDirection};
use nexus_exchange::{Client, Config};
use wiremock::matchers::{header_exists, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key("nx_test", SECRET))
}

fn dec(s: &str) -> Decimal {
    s.parse::<Decimal>().unwrap()
}

/// The sign on `amount` and the categorical `direction` must agree — they are the
/// same fact in two forms, and a consumer that summed `amount` while filtering on
/// `direction` would double-count if they disagreed.
#[tokio::test]
async fn funding_direction_agrees_with_the_sign_on_amount() {
    let server = MockServer::start().await;
    let body = serde_json::json!([
        { "market_id": "BTC-USDX-PERP", "amount": "-12.5", "direction": "paid",
          "funding_rate": "0.0001", "position_size": "2.5",
          "timestamp": 1_776_033_900_000i64 },
        { "market_id": "ETH-USDX-PERP", "amount": "3.25", "direction": "received",
          "funding_rate": "-0.00005", "position_size": "10",
          "timestamp": 1_776_030_300_000i64 }
    ]);
    Mock::given(method("GET"))
        .and(path("/funding"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let rows = authed(server.uri())
        .fetch_account_funding(None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    assert_eq!(rows[0].direction, FundingDirection::Paid);
    assert!(rows[0].amount < Decimal::ZERO, "paid must be negative");
    assert_eq!(rows[0].amount, dec("-12.5"));
    assert_eq!(rows[0].position_size, dec("2.5"));

    assert_eq!(rows[1].direction, FundingDirection::Received);
    assert!(rows[1].amount > Decimal::ZERO, "received must be positive");
    // A negative funding rate is legitimate — shorts pay longs.
    assert_eq!(rows[1].funding_rate, dec("-0.00005"));
    assert_eq!(rows[1].market_id, "ETH-USDX-PERP");
    assert_eq!(rows[1].timestamp, 1_776_030_300_000);
}

/// Omitting `limit` asks for the server default of 100, not the 1000 maximum.
/// Asserted because reusing a paginated reader's "omit means max" intuition here
/// would silently truncate a caller expecting everything.
#[tokio::test]
async fn omitting_limit_sends_no_query_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/funding"))
        .and(query_param_is_missing("limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    assert!(authed(server.uri())
        .fetch_account_funding(None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn explicit_limit_reaches_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/funding"))
        .and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    assert!(authed(server.uri())
        .fetch_account_funding(Some(MAX_ACCOUNT_FUNDING_LIMIT))
        .await
        .unwrap()
        .is_empty());
}

/// This endpoint's ceiling is 1000, distinct from the paginated readers' bounds.
/// Pinned so a shared clamp can't be introduced that rejects a legal request.
#[tokio::test]
async fn out_of_range_limits_are_rejected_locally() {
    let server = MockServer::start().await;
    assert_eq!(MAX_ACCOUNT_FUNDING_LIMIT, 1000);
    for bad in [0u32, MAX_ACCOUNT_FUNDING_LIMIT + 1] {
        assert!(authed(server.uri())
            .fetch_account_funding(Some(bad))
            .await
            .is_err());
    }
    // Nothing was signed or sent for a rejected limit.
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// No funding yet is an empty 200, not an error — a new account has none.
#[tokio::test]
async fn empty_funding_history_is_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/funding"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    assert!(authed(server.uri())
        .fetch_account_funding(None)
        .await
        .unwrap()
        .is_empty());
}
