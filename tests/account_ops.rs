use nexus_exchange::types::Decimal;
use nexus_exchange::{Client, Config};
use wiremock::matchers::{body_json, method, path};
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "balance": "110000.00" })))
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
        .and(body_json(serde_json::json!({ "address": "0xabc", "tier": "MarketMaker" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "address": "0xabc", "tier": "MarketMaker"
        })))
        .mount(&server)
        .await;
    let t = authed(server.uri()).set_account_tier("0xabc", "MarketMaker").await.unwrap();
    assert_eq!(t.tier, "MarketMaker");
}

#[tokio::test]
async fn mint_ws_token_parses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ws/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "token": "abc123" })))
        .mount(&server)
        .await;
    let tok = authed(server.uri()).mint_web_socket_token().await.unwrap();
    assert_eq!(tok.token, "abc123");
}
