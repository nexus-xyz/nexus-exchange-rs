//! Signed trading actions on the wire (ENG-20652): which requests carry the
//! `x-action-*` headers, for an agent key and for an HMAC key with an action
//! signer. The digests themselves are pinned in `src/auth/trading.rs`.

use nexus_exchange::types::{AmendOrder, Decimal, OrderRequest, Side, TimeInForce};
use nexus_exchange::{AgentSigner, Client, Config, CustomNetwork, Funds, Network};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AGENT_KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
const OWNER: &str = "0x1111111111111111111111111111111111111111";
const SUBACCOUNT: &str = "0x2222222222222222222222222222222222222222";
const HMAC_SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn network(uri: &str, domain: Option<&str>) -> Network {
    let target = CustomNetwork::new("dev", uri, Funds::Play).unwrap();
    Network::Custom(match domain {
        Some(domain) => target.with_deployment_domain(domain),
        None => target,
    })
}

fn agent() -> AgentSigner {
    AgentSigner::from_hex(AGENT_KEY)
        .unwrap()
        .with_account(OWNER)
        .unwrap()
}

/// Every request the SDK sends to a trading route, then two that are not one,
/// against a server that refuses them all. Returns each request's headers as
/// `(method path, header names)`.
async fn send_all(config: impl FnOnce(&str) -> Config) -> Vec<(String, Vec<String>)> {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({ "code": "Test", "message": "refused" })),
        )
        .mount(&server)
        .await;
    let client = Client::new(config(&server.uri()));
    let order = OrderRequest::limit(
        "BTC-USDX-PERP",
        Side::Buy,
        Decimal::new(50_000, 0),
        Decimal::new(1, 1),
        TimeInForce::Gtc,
    );
    let _ = client.create_order(&order).await;
    let _ = client.create_orders(std::slice::from_ref(&order)).await;
    let _ = client
        .edit_order(
            "o1",
            "BTC-USDX-PERP",
            &AmendOrder::new().price(Decimal::ONE),
        )
        .await;
    let _ = client.cancel_order("o1", "BTC-USDX-PERP").await;
    let _ = client.cancel_all_orders().await;
    let _ = client.cancel_orders_for_market("BTC-USDX-PERP").await;
    let _ = client.add_margin("BTC-USDX-PERP", Decimal::ONE).await;
    let _ = client.preview_order(&order).await;
    let _ = client.fetch_open_orders().await;

    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|request| {
            let mut names: Vec<String> = request
                .headers
                .keys()
                .map(|name| name.as_str().to_string())
                .filter(|name| name.starts_with("x-") && name != "x-nexus-api-version")
                .collect();
            names.sort();
            (format!("{} {}", request.method, request.url.path()), names)
        })
        .collect()
}

const TRADING: [&str; 7] = [
    "POST /orders",
    "POST /orders/batch",
    "PATCH /orders/o1",
    "DELETE /orders/o1",
    "DELETE /orders",
    "DELETE /orders",
    "POST /account/margin",
];
const OTHER: [&str; 2] = ["POST /orders/preview", "GET /orders"];

fn routes(sent: &[(String, Vec<String>)]) -> Vec<&str> {
    sent.iter().map(|(route, _)| route.as_str()).collect()
}

#[tokio::test]
async fn an_agent_key_signs_trading_routes_as_actions() {
    let sent = send_all(|uri| Config::new(network(uri, Some("devnet"))).agent_key(agent())).await;
    assert_eq!(routes(&sent), [&TRADING[..], &OTHER[..]].concat());
    for (route, names) in &sent[..TRADING.len()] {
        assert_eq!(
            names,
            &[
                "x-action-nonce",
                "x-action-signature",
                "x-action-timestamp",
                "x-agent"
            ],
            "{route}"
        );
    }
    for (route, names) in &sent[TRADING.len()..] {
        assert_eq!(
            names,
            &["x-agent", "x-nonce", "x-signature", "x-timestamp"],
            "{route}"
        );
    }
}

#[tokio::test]
async fn an_hmac_key_with_an_action_signer_carries_both() {
    let sent = send_all(|uri| {
        Config::new(network(uri, Some("devnet")))
            .api_key("nx_test", HMAC_SECRET)
            .with_action_signer(agent())
    })
    .await;
    for (route, names) in &sent[..TRADING.len()] {
        assert_eq!(
            names,
            &[
                "x-action-nonce",
                "x-action-signature",
                "x-action-timestamp",
                "x-api-key",
                "x-signature",
                "x-timestamp"
            ],
            "{route}"
        );
    }
    for (route, names) in &sent[TRADING.len()..] {
        assert_eq!(
            names,
            &["x-api-key", "x-signature", "x-timestamp"],
            "{route}"
        );
    }
}

#[tokio::test]
async fn a_subaccount_is_named_in_x_acting_account() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    let signer = agent().with_acting_account(SUBACCOUNT).unwrap();
    let client = Client::new(Config::new(network(&server.uri(), Some("devnet"))).agent_key(signer));
    client.cancel_all_orders().await.unwrap();
    let request = &server.received_requests().await.unwrap()[0];
    assert_eq!(request.headers["x-acting-account"], SUBACCOUNT);
}

/// No declared deployment domain (every built-in network today): requests are
/// signed exactly as before, so nothing breaks where the server's agent
/// verifier has no domain to check a typed action under.
#[tokio::test]
async fn without_a_deployment_domain_nothing_changes() {
    let sent = send_all(|uri| {
        Config::new(network(uri, None))
            .api_key("nx_test", HMAC_SECRET)
            .with_action_signer(agent())
    })
    .await;
    for (route, names) in &sent {
        assert_eq!(
            names,
            &["x-api-key", "x-signature", "x-timestamp"],
            "{route}"
        );
    }
    let sent = send_all(|uri| Config::new(network(uri, None)).agent_key(agent())).await;
    for (route, names) in &sent {
        assert_eq!(
            names,
            &["x-agent", "x-nonce", "x-signature", "x-timestamp"],
            "{route}"
        );
    }
}
