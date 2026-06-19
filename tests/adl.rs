//! Integration tests for the ADL (auto-deleveraging) reads: market settlement
//! history and per-account ADL history. Both are HMAC-gated server-side
//! (`hmacAuth`), so the SDK signs them — the tests assert the API-key headers
//! land on the wire and that the calls fail locally without credentials.

use nexus_exchange::{Client, Config, Error};
use wiremock::matchers::{header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Client authenticated with an HMAC API key (the ADL-read credential).
fn hmac(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

fn adl_body() -> serde_json::Value {
    serde_json::json!([{
        "market_id": "BTC-USDX-PERP",
        "target_account": "0xbankrupt",
        "bankruptcy_price": "49999.5",
        "bad_debt_absorbed_by_fund": "12.25",
        "counterparty_closures": [
            { "account_id": "0xcp", "position_closed": "0.5", "settlement_amount": "25000" }
        ],
        "sequence": 42,
        "timestamp": 1_776_033_900_000i64
    }])
}

#[tokio::test]
async fn fetch_market_adl_events_signs_and_passes_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/markets/BTC-USDX-PERP/adl-events"))
        .and(query_param("limit", "50"))
        // HMAC-gated: the signing headers must land on the wire.
        .and(header("x-api-key", "nx_test"))
        .and(header_exists("x-signature"))
        .and(header_exists("x-timestamp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(adl_body()))
        .mount(&server)
        .await;

    let events = hmac(server.uri())
        .fetch_market_adl_events("BTC-USDX-PERP", Some(50))
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].target_account, "0xbankrupt");
    assert_eq!(events[0].bankruptcy_price.to_string(), "49999.5");
    assert_eq!(events[0].counterparty_closures[0].account_id, "0xcp");
    assert_eq!(events[0].sequence, 42);
}

#[tokio::test]
async fn fetch_market_adl_events_omits_limit_when_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/markets/BTC-USDX-PERP/adl-events"))
        .and(header("x-api-key", "nx_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let events = hmac(server.uri())
        .fetch_market_adl_events("BTC-USDX-PERP", None)
        .await
        .unwrap();
    assert!(events.is_empty());
}

#[tokio::test]
async fn fetch_account_adl_history_signs_for_address() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/account/0xtrader/adl-history"))
        .and(query_param("limit", "100"))
        .and(header("x-api-key", "nx_test"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(adl_body()))
        .mount(&server)
        .await;

    let history = hmac(server.uri())
        .fetch_account_adl_history("0xtrader", Some(100))
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].market_id, "BTC-USDX-PERP");
}

#[tokio::test]
async fn adl_reads_require_credentials() {
    // Unauthenticated client: each ADL read must surface Auth before any I/O,
    // so a port that refuses connections is never even dialed.
    let client = Client::new(Config::with_base_url("http://localhost:1"));
    assert!(matches!(
        client
            .fetch_market_adl_events("BTC-USDX-PERP", None)
            .await
            .unwrap_err(),
        Error::Auth(_)
    ));
    assert!(matches!(
        client
            .fetch_account_adl_history("0xtrader", None)
            .await
            .unwrap_err(),
        Error::Auth(_)
    ));
}

#[tokio::test]
async fn fetch_account_adl_history_rejects_empty_address_locally() {
    let err = hmac("http://localhost:1".into())
        .fetch_account_adl_history("", None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidRequest(_)));
}
