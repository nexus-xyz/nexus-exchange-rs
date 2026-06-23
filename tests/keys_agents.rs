//! Integration tests for login + key-management + agent endpoints (ENG-3403).
//!
//! `POST /auth/login` is unauthenticated and yields the session token. `/keys`
//! create/delete use that session bearer token (spec `bearerAuth`); `/agents`
//! list/revoke use HMAC API-key signing (`hmacAuth`). The tests assert the
//! right credential lands on the wire and that caller-supplied path ids are
//! confined to a single, encoded segment.

use nexus_exchange::rest::LOGIN_MESSAGE;
use nexus_exchange::{Client, Config, Error, ExposeSecret};
use wiremock::matchers::{body_json, body_string, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Unauthenticated client (login needs no credentials).
fn anon(uri: String) -> Client {
    Client::new(Config::with_base_url(uri))
}

/// Client authenticated with a session bearer token (the `/keys` credential).
fn session(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).session_token("sess_tok_123"))
}

/// Client authenticated with an HMAC API key (the `/agents` credential).
fn hmac(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

#[tokio::test]
async fn login_sends_canonical_message_and_redacts_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        // The SDK fixes the message to the canonical value; only the signature
        // varies. This proves signed-bytes and sent-bytes can't drift.
        .and(body_json(serde_json::json!({
            "message": LOGIN_MESSAGE, "signature": "0xdeadbeef"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": "sometoken", "address": "0xabc"
        })))
        .mount(&server)
        .await;

    let resp = anon(server.uri()).login("0xdeadbeef").await.unwrap();
    assert_eq!(resp.token.expose_secret(), "sometoken");
    assert_eq!(resp.address, "0xabc");
    // The session token must not leak through Debug.
    assert!(!format!("{resp:?}").contains("sometoken"));
}

#[tokio::test]
async fn login_rejects_empty_signature_without_io() {
    // No server: an empty signature must fail locally before any request.
    let err = anon("http://localhost:1".into())
        .login("")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidRequest(_)));
}

#[tokio::test]
async fn create_api_key_uses_bearer_no_body_and_returns_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/keys"))
        .and(header("authorization", "Bearer sess_tok_123"))
        // POST /keys carries no request body; we sign sha256("") and send none.
        .and(body_string(""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "key_id": "nx_new", "secret": "deadbeefsecret", "tier": "Pro"
        })))
        .mount(&server)
        .await;

    let created = session(server.uri()).create_api_key().await.unwrap();
    assert_eq!(created.key_id, "nx_new");
    assert_eq!(created.secret.expose_secret(), "deadbeefsecret");
    assert_eq!(created.tier.as_deref(), Some("Pro"));
    // The secret must not leak through Debug even after a real round-trip.
    assert!(!format!("{created:?}").contains("deadbeefsecret"));
}

#[tokio::test]
async fn delete_api_key_sends_bearer_delete() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/keys/nx_old"))
        .and(header("authorization", "Bearer sess_tok_123"))
        .and(body_string(""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .mount(&server)
        .await;

    session(server.uri())
        .delete_api_key("nx_old")
        .await
        .unwrap();
}

#[tokio::test]
async fn fetch_agents_is_hmac_signed_and_parses_camel_case() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/agents"))
        .and(header("x-api-key", "nx_test"))
        .and(header_exists("x-signature"))
        .and(header_exists("x-timestamp"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "address": "0xagent", "expiresAt": 1_776_033_900_000i64,
                "registeredAt": 1_776_000_000_000i64, "label": "bot-1"
            }])),
        )
        .mount(&server)
        .await;

    let agents = hmac(server.uri()).fetch_agents().await.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].address, "0xagent");
    assert_eq!(agents[0].expires_at, 1_776_033_900_000);
    assert_eq!(agents[0].label.as_deref(), Some("bot-1"));
}

#[tokio::test]
async fn revoke_agent_is_hmac_signed_delete_with_no_body() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/agents/0xagent"))
        .and(header_exists("x-signature"))
        .and(body_string(""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .mount(&server)
        .await;

    hmac(server.uri()).revoke_agent("0xagent").await.unwrap();
}

#[tokio::test]
async fn key_and_agent_endpoints_require_credentials() {
    // Unauthenticated client: each method must surface Auth before any I/O,
    // so a port that refuses connections is never even dialed.
    let client = Client::new(Config::with_base_url("http://localhost:1"));
    assert!(matches!(
        client.create_api_key().await.unwrap_err(),
        Error::Auth(_)
    ));
    assert!(matches!(
        client.delete_api_key("nx_old").await.unwrap_err(),
        Error::Auth(_)
    ));
    assert!(matches!(
        client.fetch_agents().await.unwrap_err(),
        Error::Auth(_)
    ));
    assert!(matches!(
        client.revoke_agent("0xagent").await.unwrap_err(),
        Error::Auth(_)
    ));
}

#[tokio::test]
async fn path_parameters_cannot_traverse_to_another_route() {
    // A malicious id containing `../` must not be normalized into a different
    // route. Only the traversal *target* (`/account`) is mounted: if the id
    // were interpolated raw, `/keys/../account` would normalize onto it and
    // return the sentinel 200. Because the segment is percent-encoded, the
    // request stays on `/keys/<encoded>`, never matches, and surfaces an error.
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/account"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "reached": "account" })),
        )
        .mount(&server)
        .await;

    match session(server.uri()).delete_api_key("../account").await {
        Err(Error::Api { .. }) => {}
        Err(other) => panic!("expected Api error, got {other:?}"),
        Ok(v) => panic!("traversal reached another route: {v:?}"),
    }
}

#[tokio::test]
async fn empty_path_id_is_rejected_locally() {
    let err = session("http://localhost:1".into())
        .delete_api_key("")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidRequest(_)));
}
