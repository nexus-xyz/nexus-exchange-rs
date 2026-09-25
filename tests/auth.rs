use std::sync::Arc;

use nexus_exchange::{AgentSigner, Client, Config, Error, EthSigner, Nonce};
use secrecy::ExposeSecret;
use wiremock::matchers::{body_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Canonical Hardhat/ethers account #0 — a published, externally verifiable
// keypair, so the wire `wallet`/`address` values below are deterministic.
const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const TEST_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

#[tokio::test]
#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
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
#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
async fn signed_request_without_credentials_errors() {
    let client = Client::new(Config::with_base_url("http://localhost:1"));
    match client.fetch_api_keys().await.unwrap_err() {
        Error::Terminal(nexus_exchange::TerminalError::Credentials(_)) => {}
        other => panic!("expected Auth error, got {other:?}"),
    }
}

#[tokio::test]
#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
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
#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
async fn register_agent_posts_eip712_body_and_parses() {
    let server = MockServer::start().await;
    let agent = "0x1234567890abcdef1234567890abcdef12345678";
    let registration = signer()
        .register_agent(
            agent,
            1_782_000_000_000,
            1,
            20056,
            &nexus_exchange::Network::Local,
            Some("my-bot".into()),
        )
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

/// A frozen clock, so the signed request is byte-deterministic.
#[derive(Debug)]
struct FixedClock(u64);

impl Nonce for FixedClock {
    fn next(&self) -> u64 {
        self.0
    }
}

// End to end over the wire: the four agent headers the client actually sends
// for `GET /keys` equal those produced by the exchange frontend's reference
// signer (`signRequestHeaders`, `@noble/curves`) for the same key, timestamp
// and nonce. The first nonce a signer issues is its timestamp.
#[tokio::test]
#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
async fn agent_key_request_sends_reference_signed_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/keys"))
        .and(header(
            "x-agent",
            "0x1a642f0e3c3af545e7acbd38b07251b3990914f1",
        ))
        .and(header("x-timestamp", "1776033900000"))
        .and(header("x-nonce", "1776033900000"))
        .and(header(
            "x-signature",
            "0x9b198e523e2027f6c6d071a8831952092ab4ae18e8592e9a038d1e5193e6078b\
             70a9332c37197a2ce4c3825680d127800b35f885af36db55d5827869bbdab3131b",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let agent =
        AgentSigner::from_hex("0x0101010101010101010101010101010101010101010101010101010101010101")
            .unwrap();
    let client = Client::new(
        Config::with_base_url(server.uri())
            .agent_key(agent)
            .with_nonce(Arc::new(FixedClock(1_776_033_900_000))),
    );
    assert!(client.fetch_api_keys().await.unwrap().is_empty());
}
