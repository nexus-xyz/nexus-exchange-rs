//! Retry-layer behavior: only transient classes are retried, with bounded
//! attempts. Backoff delays are kept tiny and jitter disabled so the suite
//! stays fast and deterministic.

use std::time::Duration;

use nexus_exchange::{Client, Config, Error, RateLimit, RetryConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `max_retries: 3` → up to 4 attempts total, with negligible backoff.
fn fast_retry() -> RetryConfig {
    RetryConfig {
        max_retries: 3,
        min_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
        factor: 2.0,
        jitter: false,
        max_total_delay: None,
    }
}

fn client(uri: String, retry: RetryConfig) -> Client {
    Client::new(Config::with_base_url(uri).with_retry(retry))
}

fn health_body() -> serde_json::Value {
    serde_json::json!({
        "events_received": 1, "fills_total": 2, "uptime_seconds": 3, "connected": true
    })
}

#[tokio::test]
async fn retries_transient_5xx_then_succeeds() {
    let server = MockServer::start().await;
    // First two attempts get a 503, then a 200. wiremock matches the
    // highest-priority mock that still has responses left, so the priority-1
    // 503 mock serves its 2 allotted responses and the next request falls
    // through to the priority-2 200 mock.
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(health_body()))
        .with_priority(2)
        .mount(&server)
        .await;

    let health = client(server.uri(), fast_retry())
        .health_check()
        .await
        .expect("should recover after transient 503s");
    assert_eq!(health.events_received, 1);
}

#[tokio::test]
async fn retries_transient_408_then_succeeds() {
    let server = MockServer::start().await;
    // 408 (Request Timeout) is transient: the first two attempts get a 408,
    // then the request falls through to the 200.
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(408))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(health_body()))
        .with_priority(2)
        .mount(&server)
        .await;

    let health = client(server.uri(), fast_retry())
        .health_check()
        .await
        .expect("should recover after transient 408s");
    assert_eq!(health.events_received, 1);
}

#[tokio::test]
async fn per_attempt_timeout_is_transient_and_retried() {
    let server = MockServer::start().await;
    // The first attempt's response is delayed past the per-request timeout, so
    // it surfaces as a transient timeout and is retried; the next attempt falls
    // through to the fast 200.
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(health_body()))
        .with_priority(2)
        .mount(&server)
        .await;

    let cfg = Config::with_base_url(server.uri())
        .with_timeout(Duration::from_millis(100))
        .with_retry(fast_retry());
    let health = Client::new(cfg)
        .health_check()
        .await
        .expect("should recover after a timed-out attempt");
    assert_eq!(health.events_received, 1);
}

#[tokio::test]
async fn retries_exhaust_then_surface_last_error() {
    let server = MockServer::start().await;
    // 1 initial attempt + 3 retries = 4 calls, all 503.
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "code": "unavailable", "message": "try later"
        })))
        .expect(4)
        .mount(&server)
        .await;

    let err = client(server.uri(), fast_retry())
        .health_check()
        .await
        .unwrap_err();
    match err {
        Error::Api { status, code, .. } => {
            assert_eq!(status, 503);
            assert_eq!(code, "unavailable");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    // `expect(4)` is asserted on drop.
}

#[tokio::test]
async fn does_not_retry_non_transient_4xx() {
    let server = MockServer::start().await;
    // A 400 is deterministic: it must be tried exactly once, never retried.
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "code": "bad_request", "message": "nope"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let err = client(server.uri(), fast_retry())
        .health_check()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api { status: 400, .. }));
}

#[tokio::test]
async fn does_not_retry_429_owned_by_rate_limit_layer() {
    let server = MockServer::start().await;
    // 429 is owned end-to-end by the rate-limit layer, not this generic retry
    // layer. With the rate limiter's own 429 retries turned off, a 429 is tried
    // exactly once and surfaces as `Error::RateLimited` — proving the generic
    // transient-retry layer does not *also* back off on it (otherwise we'd see
    // extra attempts and an `Error::Api`).
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "code": "rate_limited", "message": "slow down"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = Config::with_base_url(server.uri())
        .with_retry(fast_retry())
        .with_rate_limit(RateLimit::new(10.0).with_max_retries(0));
    let err = Client::new(cfg).health_check().await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { .. }));
}

#[tokio::test]
async fn disabled_retry_makes_a_single_attempt() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let err = client(server.uri(), RetryConfig::disabled())
        .health_check()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api { status: 503, .. }));
}
