use nexus_exchange::types::Decimal;
use nexus_exchange::{Client, Config};
use wiremock::matchers::{body_json, body_string, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
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
        .and(path("/account/credit"))
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
        // signs an empty body and sends none — verify no body is transmitted.
        .and(body_string(""))
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "o1", "market_id": "BTC-USDX-PERP", "side": "Buy",
            "order_type": "Limit", "time_in_force": "GTC"
        })))
        .mount(&server)
        .await;
    let o = authed(server.uri()).fetch_order("o1").await.unwrap();
    assert_eq!(o.account_id, "");
    assert_eq!(o.quantity.to_string(), "0");
    assert_eq!(o.filled_qty.to_string(), "0");
    assert_eq!(o.status, "");
    assert_eq!(o.created_at, 0);
}
