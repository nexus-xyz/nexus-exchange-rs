use nexus_exchange::{Client, Config, Error};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
