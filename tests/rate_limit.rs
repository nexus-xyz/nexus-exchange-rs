use nexus_exchange::{Client, Config, Error, RateLimit};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a client against `uri` with a fast, deterministic rate-limit policy:
/// proactive pacing off (so tests don't sleep on the token bucket) but reactive
/// `429` retries bounded at `max_retries`.
///
/// Credentials are attached so the signed `/account/rate-limit` endpoint works;
/// the public-endpoint tests don't sign, so they're simply unused there.
fn client(uri: String, max_retries: u32) -> Client {
    let cfg = Config::with_base_url(uri)
        .api_key(
            "nx",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        )
        .with_rate_limit(
            RateLimit::new(10.0)
                .with_limiter_enabled(false)
                .with_max_retries(max_retries),
        );
    Client::new(cfg)
}

#[tokio::test]
async fn retries_on_429_then_succeeds() {
    let server = MockServer::start().await;

    // Registered first => lower precedence: serves once the 429 mock is spent.
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "events_received": 1, "fills_total": 2, "uptime_seconds": 3, "connected": true
        })))
        .mount(&server)
        .await;

    // Registered last => higher precedence, but only good for one response.
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let health = client(server.uri(), 3).health_check().await.unwrap();
    assert_eq!(health.events_received, 1);
    assert!(health.connected);
}

#[tokio::test]
async fn exhausting_retries_yields_rate_limited_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/markets"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .insert_header("X-RateLimit-Limit", "5")
                .insert_header("X-RateLimit-Remaining", "0"),
        )
        .mount(&server)
        .await;

    let err = client(server.uri(), 2).fetch_markets().await.unwrap_err();
    match err {
        Error::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(0)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn fetch_rate_limit_status_parses_and_returns() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/account/rate-limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tier": "pro", "limit": 50, "remaining": 12, "reset_at_ms": 1776033900000i64
        })))
        .mount(&server)
        .await;

    let status = client(server.uri(), 3)
        .fetch_rate_limit_status()
        .await
        .unwrap();
    assert_eq!(status.tier, "pro");
    assert_eq!(status.limit, Some(50));
    assert_eq!(status.remaining, Some(12));
    assert_eq!(status.reset_at_ms, Some(1776033900000));
}

#[tokio::test]
async fn unlimited_tier_status_has_null_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/account/rate-limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tier": "unlimited", "limit": null, "remaining": null, "reset_at_ms": null
        })))
        .mount(&server)
        .await;

    let status = client(server.uri(), 3)
        .fetch_rate_limit_status()
        .await
        .unwrap();
    assert_eq!(status.tier, "unlimited");
    assert!(status.limit.is_none());
    assert!(status.remaining.is_none());
}
