use nexus_exchange::{Client, Config, Error, EthSigner};
use secrecy::ExposeSecret;
use wiremock::matchers::{body_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Canonical Hardhat/ethers account #0 — a published, externally verifiable
// keypair, so the wire `wallet`/`address` values below are deterministic.
const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const TEST_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

#[tokio::test]
async fn signed_request_sends_hmac_headers_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/keys"))
        .and(header("x-api-key", "nx_test"))
        .and(header_exists("x-signature"))
        .and(header_exists("x-timestamp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "key_id": "nx_test", "tier": "Pro" }
        ])))
        .mount(&server)
        .await;

    let client = Client::new(Config::with_base_url(server.uri()).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ));
    let keys = client.fetch_api_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].tier, "Pro");
}

#[tokio::test]
async fn signed_request_without_credentials_errors() {
    let client = Client::new(Config::with_base_url("http://localhost:1"));
    match client.fetch_api_keys().await.unwrap_err() {
        Error::Auth(_) => {}
        other => panic!("expected Auth error, got {other:?}"),
    }
}

#[tokio::test]
async fn sign_in_posts_eip191_body_and_parses_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        // The signed body carries the exact fixed login message; the signature
        // is a 0x-prefixed 65-byte hex string (132 chars).
        .and(body_json(serde_json::json!({
            "message": "Sign in to Nexus Exchange",
            "signature": signer().sign_in().unwrap().signature,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": "a1b2c3d4e5f6",
            "address": TEST_ADDR,
        })))
        .mount(&server)
        .await;

    let client = Client::new(Config::with_base_url(server.uri()));
    let resp = client.sign_in(&signer()).await.unwrap();
    assert_eq!(resp.token.expose_secret(), "a1b2c3d4e5f6");
    assert_eq!(resp.address, TEST_ADDR);
}

#[tokio::test]
async fn register_agent_posts_eip712_body_and_parses() {
    let server = MockServer::start().await;
    let agent = "0x1234567890abcdef1234567890abcdef12345678";
    let registration = signer()
        .register_agent(agent, 1_782_000_000_000, 1, 393, Some("my-bot".into()))
        .unwrap();

    Mock::given(method("POST"))
        .and(path("/agents/register"))
        .and(body_json(serde_json::json!({
            "wallet": TEST_ADDR,
            "agent": agent,
            "expires_at": 1_782_000_000_000u64,
            "nonce": 1,
            "signature": registration.signature,
            "label": "my-bot",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "agent_address": agent,
            "expires_at": 1_782_000_000_000u64,
        })))
        .mount(&server)
        .await;

    let client = Client::new(Config::with_base_url(server.uri()));
    let resp = client.register_agent(&registration).await.unwrap();
    assert_eq!(resp.agent_address, agent);
    assert_eq!(resp.expires_at, 1_782_000_000_000);
}

fn signer() -> EthSigner {
    EthSigner::from_hex(TEST_KEY).unwrap()
}
