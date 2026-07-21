use nexus_exchange::types::Decimal;
use nexus_exchange::{Client, Config, Error, Network, TerminalError};
use wiremock::matchers::{body_json, body_string, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key("nx_test", SECRET))
}

fn dec(s: &str) -> Decimal {
    s.parse::<Decimal>().unwrap()
}

#[tokio::test]
async fn deposit_sends_amount_and_parses_balance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/account/deposit"))
        .and(body_json(serde_json::json!({ "amount": "10000" })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "balance": "110000.00" })),
        )
        .mount(&server)
        .await;
    let r = authed(server.uri())
        .deposit("10000".parse::<Decimal>().unwrap())
        .await
        .unwrap();
    assert_eq!(r.balance.to_string(), "110000.00");
}

#[tokio::test]
async fn claim_credit_without_amount_sends_empty_object() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/account/credit"))
        .and(body_json(serde_json::json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "amount": "500", "credited_today": "500", "daily_limit": "500"
        })))
        .mount(&server)
        .await;
    let r = authed(server.uri()).claim_credit(None).await.unwrap();
    assert_eq!(r.daily_limit.to_string(), "500");
}

#[tokio::test]
async fn deposit_rejects_non_positive_amount() {
    // A zero/negative deposit is rejected locally — no request is sent (the URL
    // is unroutable, so reaching the network would itself fail the test).
    let err = authed("http://127.0.0.1:1".to_string())
        .deposit(dec("0"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn fund_on_production_refuses_to_move_real_collateral() {
    // The critical safety property: on a real-money network, fund() must NOT
    // silently deposit. It rejects locally and points the caller at deposit().
    // No mock server: the guard fires before any request would be sent.
    let client = Client::new(Config::new(Network::Stable).api_key("nx_test", SECRET));
    let err = client.fund(dec("1000")).await.unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn fund_with_unknown_network_refuses() {
    // Built from a raw base URL, so the SDK can't tell real-money from testnet:
    // fund() refuses rather than guess. (authed() uses Config::with_base_url.)
    let err = authed("http://127.0.0.1:1".to_string())
        .fund(dec("1000"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn fund_rejects_non_positive_amount() {
    let err = Client::new(Config::new(Network::Beta).api_key("nx_test", SECRET))
        .fund(dec("0"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Terminal(TerminalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn set_account_tier_uses_put_with_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/admin/tiers"))
        .and(body_json(
            serde_json::json!({ "address": "0xabc", "tier": "MarketMaker" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "address": "0xabc", "tier": "MarketMaker"
        })))
        .mount(&server)
        .await;
    let t = authed(server.uri())
        .set_account_tier("0xabc", "MarketMaker")
        .await
        .unwrap();
    assert_eq!(t.tier, "MarketMaker");
}

#[tokio::test]
async fn mint_ws_token_parses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ws/token"))
        // The empty body is signed as-is, while its zero length is explicit on
        // the wire so HTTP gateways accept the POST.
        .and(body_string(""))
        .and(header("content-length", "0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "token": "abc123" })),
        )
        .mount(&server)
        .await;
    let tok = authed(server.uri()).mint_web_socket_token().await.unwrap();
    assert_eq!(tok.token, "abc123");
}

#[tokio::test]
async fn fetch_withdrawals_parses_and_is_signed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/withdrawals"))
        .and(header_exists("x-signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": "w1", "amount": "250.00", "timestamp": 1776033900000i64, "status": "pending"
            }])),
        )
        .mount(&server)
        .await;
    let w = authed(server.uri()).fetch_withdrawals().await.unwrap();
    assert_eq!(w[0].amount.to_string(), "250.00");
    assert_eq!(w[0].status, "pending");
}

#[tokio::test]
async fn fetch_tier_overrides_parses_and_is_signed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/admin/tiers"))
        .and(header_exists("x-signature"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "address": "0xabc", "tier": "MarketMaker"
            }])),
        )
        .mount(&server)
        .await;
    let t = authed(server.uri()).fetch_tier_overrides().await.unwrap();
    assert_eq!(t[0].tier, "MarketMaker");
}

#[tokio::test]
async fn reset_account_tier_signed_delete_sends_no_body() {
    // DELETE with the no-body signing path: signs sha256("") and sends nothing.
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/admin/tiers/0xabc"))
        .and(header_exists("x-signature"))
        .and(body_string(""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .mount(&server)
        .await;
    authed(server.uri())
        .reset_account_tier("0xabc")
        .await
        .unwrap();
}

#[tokio::test]
async fn fetch_order_defaults_omitted_optional_fields() {
    // The spec marks every Order field optional; a slim payload carrying only
    // the identity + enum fields must decode, defaulting the rest rather than
    // failing.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/orders/o1"))
        .and(wiremock::matchers::query_param(
            "market_id",
            "BTC-USDX-PERP",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "o1", "market_id": "BTC-USDX-PERP", "side": "Buy",
            "order_type": "Limit", "time_in_force": "GTC"
        })))
        .mount(&server)
        .await;
    let o = authed(server.uri())
        .fetch_order("o1", "BTC-USDX-PERP")
        .await
        .unwrap();
    assert_eq!(o.account_id, "");
    assert_eq!(o.quantity.to_string(), "0");
    assert_eq!(o.filled_qty.to_string(), "0");
    assert_eq!(o.status, "");
    assert_eq!(o.created_at, 0);
}
