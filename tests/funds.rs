//! The spec'd funds surface: `GET /deposits`, `POST /deposits`, `POST /faucet`.
//!
//! Every test starts from a `Client` method against a mock server, so it fails if
//! the endpoint is unreachable from the public API rather than only if a type is
//! wrong — the lesson from the `Paginator` work, where a green unit suite sat
//! behind a method nothing on `Client` returned.

use nexus_exchange::types::{Decimal, FundsKind, FundsStatus};
use nexus_exchange::{Client, Config};
use wiremock::matchers::{
    body_json, header_exists, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key("nx_test", SECRET))
}

fn dec(s: &str) -> Decimal {
    s.parse::<Decimal>().unwrap()
}

/// The row type is shared across kinds, so `GET /deposits` returns withdrawals
/// and faucet grants too. A caller that assumed otherwise would double-count a
/// withdrawal as an inflow, which is why `kind` is a typed enum and asserted here.
#[tokio::test]
async fn fetch_deposits_returns_every_kind_not_just_deposits() {
    let server = MockServer::start().await;
    let body = serde_json::json!([
        { "id": 3, "kind": "faucet", "account": "0xabc", "amount": "100.5",
          "asset": "USDX", "timestamp": 1_776_033_900_000i64, "status": "confirmed",
          "tx_hash": null },
        { "id": 2, "kind": "withdrawal", "account": "0xabc", "amount": "25",
          "asset": "USDX", "timestamp": 1_776_033_800_000i64, "status": "pending",
          "tx_hash": "0xdeadbeef" },
        { "id": 1, "kind": "deposit", "account": "0xabc", "amount": "1000",
          "asset": "USDX", "timestamp": 1_776_033_700_000i64, "status": "failed",
          "tx_hash": "0xfeed" }
    ]);
    Mock::given(method("GET"))
        .and(path("/deposits"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let rows = authed(server.uri()).fetch_deposits(None).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].kind, FundsKind::Faucet);
    assert_eq!(rows[1].kind, FundsKind::Withdrawal);
    assert_eq!(rows[2].kind, FundsKind::Deposit);
    // All three settlement states decode.
    assert_eq!(rows[0].status, FundsStatus::Confirmed);
    assert_eq!(rows[1].status, FundsStatus::Pending);
    assert_eq!(rows[2].status, FundsStatus::Failed);
    // Amounts are lossless decimals, not floats.
    assert_eq!(rows[0].amount, dec("100.5"));
    // A chain-less row is None, not an empty string.
    assert_eq!(rows[0].tx_hash, None);
    assert_eq!(rows[1].tx_hash.as_deref(), Some("0xdeadbeef"));
    assert_eq!(rows[2].asset, "USDX");
    assert_eq!(rows[0].id, 3);
    assert_eq!(rows[0].timestamp, 1_776_033_900_000);
}

#[tokio::test]
async fn fetch_deposits_sends_limit_and_omits_it_when_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/deposits"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    assert!(authed(server.uri())
        .fetch_deposits(Some(50))
        .await
        .unwrap()
        .is_empty());

    let bare = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/deposits"))
        .and(query_param_is_missing("limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&bare)
        .await;
    assert!(authed(bare.uri())
        .fetch_deposits(None)
        .await
        .unwrap()
        .is_empty());
}

/// Over-max and zero are rejected locally, before anything is signed or sent.
#[tokio::test]
async fn fetch_deposits_rejects_out_of_range_limit_without_sending() {
    let server = MockServer::start().await;
    for bad in [0u32, 101] {
        assert!(authed(server.uri())
            .fetch_deposits(Some(bad))
            .await
            .is_err());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_deposit_sends_amount_and_parses_authoritative_balance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/deposits"))
        .and(body_json(serde_json::json!({ "amount": "250.75" })))
        .and(header_exists("x-signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "balance": "1250.75" })),
        )
        .mount(&server)
        .await;

    let res = authed(server.uri())
        .create_deposit(dec("250.75"), None)
        .await
        .unwrap();
    assert_eq!(res.balance, dec("1250.75"));
}

/// `asset` is only sent when supplied — the server defaults it to USDX, so an
/// unconditional key would override that default with a hardcoded guess.
#[tokio::test]
async fn create_deposit_includes_asset_only_when_supplied() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/deposits"))
        .and(body_json(
            serde_json::json!({ "amount": "10", "asset": "ETH" }),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "balance": "10" })),
        )
        .mount(&server)
        .await;

    let res = authed(server.uri())
        .create_deposit(dec("10"), Some("ETH"))
        .await
        .unwrap();
    assert_eq!(res.balance, dec("10"));
}

#[tokio::test]
async fn create_deposit_rejects_non_positive_and_blank_asset_locally() {
    let server = MockServer::start().await;
    for bad in ["0", "-5"] {
        assert!(authed(server.uri())
            .create_deposit(dec(bad), None)
            .await
            .is_err());
    }
    assert!(authed(server.uri())
        .create_deposit(dec("1"), Some("   "))
        .await
        .is_err());
    // Nothing was signed or sent for any rejected input.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn claim_faucet_parses_amount_and_next_available_time() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/faucet"))
        .and(header_exists("x-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "amount": "500", "available_at_ms": 1_776_120_300_000i64 }),
        ))
        .mount(&server)
        .await;

    let res = authed(server.uri()).claim_faucet().await.unwrap();
    assert_eq!(res.amount, dec("500"));
    assert_eq!(res.available_at_ms, 1_776_120_300_000);
}

/// A claim inside the cooldown is a `429`, which must surface as an error rather
/// than a zero-amount success.
#[tokio::test]
async fn faucet_cooldown_429_is_an_error_not_a_zero_grant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/faucet"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .mount(&server)
        .await;

    assert!(authed(server.uri()).claim_faucet().await.is_err());
}
