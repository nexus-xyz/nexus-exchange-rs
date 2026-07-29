//! Cursor pagination end-to-end, driven through `Client` against a mock server.
//!
//! `src/rest/pagination.rs` has always had unit tests, but they exercise
//! `Paginator` against hand-written closures — they pass whether or not any
//! endpoint method returns one. These tests are the missing half: every walk here
//! starts from a **`Client` method**, so they fail if the paginator is not
//! reachable from the public API, if the `cursor` query parameter is not sent, or
//! if the `X-Next-Cursor` response header is not read.
//!
//! Termination is the behaviour that matters most, so all four endings are
//! pinned: a cursor present (keep going), the header absent (done, not an error),
//! an empty page that still carries a cursor (keep going), and a cursor that does
//! not advance (stop, never spin).

use futures_util::StreamExt;
use nexus_exchange::rest::{MAX_FILLS_LIMIT, MAX_TRADES_LIMIT};
use nexus_exchange::{Client, Config};
use wiremock::matchers::{header_exists, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TRADES_PATH: &str = "/api/v1/markets/BTC-USDX-PERP/trades";
const FILLS_PATH: &str = "/api/v1/fills";

fn public(uri: String) -> Client {
    Client::new(Config::with_base_url(uri))
}

fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

fn trade(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "symbol": "BTC-USDX-PERP", "price": 50000.0, "amount": 1.0,
        "cost": 50000.0, "side": "buy", "timestamp": 1776033900000i64,
        "datetime": "2026-04-12T00:05:00Z", "is_liquidation": false, "info": {}
    })
}

fn fill(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "order_id": "o1", "market_id": "BTC-USDX-PERP", "side": "buy",
        "price": "50000", "size": "1", "fee": "0.5", "taker_or_maker": "taker",
        "timestamp": 1776033900000i64, "is_liquidation": false
    })
}

/// A page response: the body, plus `X-Next-Cursor` when there is a next page.
fn page(body: serde_json::Value, next_cursor: Option<&str>) -> ResponseTemplate {
    let template = ResponseTemplate::new(200).set_body_json(body);
    match next_cursor {
        Some(cursor) => template.insert_header("x-next-cursor", cursor),
        None => template,
    }
}

// -- the paginator is reachable from `Client`, and it pages -------------------

/// The test the existing unit tests could not fail: a `Client` method hands back
/// a paginator, and that paginator walks more than one page of real HTTP.
#[tokio::test]
async fn client_trades_paginator_walks_every_page() {
    let server = MockServer::start().await;
    // Page 1: two trades and a cursor.
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(
            serde_json::json!([trade("t1"), trade("t2")]),
            Some("cur-2"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    // Page 2: one trade and NO cursor, which ends the walk.
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param("cursor", "cur-2"))
        .respond_with(page(serde_json::json!([trade("t3")]), None))
        .expect(1)
        .mount(&server)
        .await;

    let trades = public(server.uri())
        .fetch_trades_paginated("BTC-USDX-PERP")
        .unwrap()
        .all()
        .await
        .unwrap();

    let ids: Vec<&str> = trades.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["t1", "t2", "t3"]);
    // Exactly two requests: no speculative fetch past the final page.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn client_fills_paginator_pages_and_signs_every_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(serde_json::json!([fill("f1")]), Some("cur-b")))
        .expect(1)
        .mount(&server)
        .await;
    // The cursor rides in the query, so page 2 is signed over a *different*
    // canonical string — each page is independently signed.
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param("cursor", "cur-b"))
        .and(header_exists("x-signature"))
        .respond_with(page(serde_json::json!([fill("f2")]), None))
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades_paginated()
        .all()
        .await
        .unwrap();

    let ids: Vec<&str> = fills.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["f1", "f2"]);
}

#[tokio::test]
async fn paginator_next_page_exposes_the_cursor_for_manual_paging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([fill("f1")]), Some("cur-b")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param("cursor", "cur-b"))
        .respond_with(page(serde_json::json!([fill("f2")]), None))
        .mount(&server)
        .await;

    let mut pager = authed(server.uri()).fetch_my_trades_paginated();

    let first = pager.next_page().await.unwrap().unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(!first.is_last());
    // The cursor is what a resumable job persists.
    assert_eq!(first.next_cursor.as_ref().unwrap().as_str(), "cur-b");

    let second = pager.next_page().await.unwrap().unwrap();
    assert!(second.is_last());
    assert!(second.next_cursor.is_none());

    assert!(pager.next_page().await.unwrap().is_none());
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

/// `starting_after` has to reach the wire as `cursor=` for a resumed walk to
/// actually resume — the unit tests only prove it reaches the closure.
#[tokio::test]
async fn starting_after_sends_the_cursor_on_the_first_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param("cursor", "saved"))
        .respond_with(page(serde_json::json!([fill("f9")]), None))
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades_paginated()
        .starting_after("saved")
        .all()
        .await
        .unwrap();
    assert_eq!(fills.len(), 1);
}

#[tokio::test]
async fn cursor_is_sent_back_verbatim() {
    // Cursors are opaque: a token with URL-hostile bytes must survive
    // percent-encoding intact, and (on the signed route) be signed as sent.
    let opaque = "eyJvIjoxMH0=+/";
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([fill("f1")]), Some(opaque)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param("cursor", opaque))
        .respond_with(page(serde_json::json!([]), None))
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades_paginated()
        .all()
        .await
        .unwrap();
    assert_eq!(fills.len(), 1);
}

#[tokio::test]
async fn into_stream_walks_pages_from_a_client_method() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([trade("t1")]), Some("cur-2")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param("cursor", "cur-2"))
        .respond_with(page(serde_json::json!([trade("t2")]), None))
        .mount(&server)
        .await;

    let stream = public(server.uri())
        .fetch_trades_paginated("BTC-USDX-PERP")
        .unwrap()
        .into_stream();
    let ids: Vec<String> = stream.map(|t| t.unwrap().id).collect().await;
    assert_eq!(ids, vec!["t1", "t2"]);
}

// -- termination --------------------------------------------------------------

#[tokio::test]
async fn absent_next_cursor_header_ends_the_walk() {
    // No `X-Next-Cursor` is the documented last-page signal: one request, no
    // error. A client that treated it as an error, or retried, would be wrong.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .respond_with(page(serde_json::json!([trade("t1")]), None))
        .expect(1)
        .mount(&server)
        .await;

    let trades = public(server.uri())
        .fetch_trades_paginated("BTC-USDX-PERP")
        .unwrap()
        .all()
        .await
        .unwrap();
    assert_eq!(trades.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn empty_first_page_terminates_without_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .respond_with(page(serde_json::json!([]), None))
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades_paginated()
        .all()
        .await
        .unwrap();
    assert!(fills.is_empty());
}

#[tokio::test]
async fn empty_page_with_a_cursor_keeps_paging() {
    // An empty page that still advertises a cursor is NOT the end. Stopping here
    // would silently truncate a walk across a sparse window.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([]), Some("cur-2")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param("cursor", "cur-2"))
        .respond_with(page(serde_json::json!([fill("f9")]), None))
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades_paginated()
        .all()
        .await
        .unwrap();
    assert_eq!(fills.len(), 1);
}

#[tokio::test]
async fn blank_next_cursor_header_counts_as_absent() {
    // A present-but-empty header cannot be sent back meaningfully — passing it on
    // would re-request the first page forever. Treat it as "no next page".
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([fill("f1")]))
                .insert_header("x-next-cursor", "   "),
        )
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades_paginated()
        .all()
        .await
        .unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// A server that echoes back the cursor it was given cannot advance. The
/// paginator returns that page and stops rather than re-issuing the identical
/// request forever.
///
/// Note the deliberate fleet difference: this SDK **stops silently**, while
/// `nexus-exchange-py` raises `PaginationError` on the same condition (a Python
/// generator feeding `list(...)` has no other way to signal a truncated walk).
/// Here the caller still gets the last page's `next_cursor` from `next_page`, so
/// the stall is observable without an error type.
#[tokio::test]
async fn repeated_cursor_stops_instead_of_spinning() {
    let server = MockServer::start().await;
    // Every request — with or without a cursor — answers with the same cursor.
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .respond_with(page(serde_json::json!([fill("f1")]), Some("stuck")))
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades_paginated()
        .starting_after("stuck")
        .all()
        .await
        .unwrap();

    // One page, one request: the identical request was never re-issued.
    assert_eq!(fills.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn max_pages_bounds_an_endlessly_advancing_server() {
    // A server that keeps handing back *new* cursors is indistinguishable from a
    // genuinely long history, so the caller's bound is what stops the walk.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([trade("t")]), Some("always-new")))
        .mount(&server)
        .await;
    // Each request is matched on the cursor it carries and answered with a *new*
    // one, so the cursor keeps advancing and `max_pages` — not the
    // repeated-cursor guard — is what stops the walk.
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param("cursor", "always-new"))
        .respond_with(page(serde_json::json!([trade("t")]), Some("always-new-2")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param("cursor", "always-new-2"))
        .respond_with(page(serde_json::json!([trade("t")]), Some("always-new-3")))
        .mount(&server)
        .await;

    let trades = public(server.uri())
        .fetch_trades_paginated("BTC-USDX-PERP")
        .unwrap()
        .max_pages(3)
        .all()
        .await
        .unwrap();
    assert_eq!(trades.len(), 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn max_pages_zero_issues_no_request() {
    let server = MockServer::start().await;
    let trades = public(server.uri())
        .fetch_trades_paginated("BTC-USDX-PERP")
        .unwrap()
        .max_pages(0)
        .all()
        .await
        .unwrap();
    assert!(trades.is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

// -- page size (`limit`) bounds ----------------------------------------------

#[tokio::test]
async fn page_size_is_sent_as_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(TRADES_PATH))
        .and(query_param("limit", "250"))
        .respond_with(page(serde_json::json!([trade("t1")]), None))
        .expect(1)
        .mount(&server)
        .await;

    public(server.uri())
        .fetch_trades_paginated("BTC-USDX-PERP")
        .unwrap()
        .page_size(250)
        .all()
        .await
        .unwrap();
}

/// The per-endpoint maxima from spec v0.7.2. Both routes this SDK implements cap
/// `limit` at 1000 — well above the `366` that belongs to the *unpaginated*
/// `/account/portfolio-history` and must not be applied here.
#[tokio::test]
async fn page_size_at_the_endpoint_maximum_is_accepted() {
    assert_eq!(MAX_TRADES_LIMIT, 1000);
    assert_eq!(MAX_FILLS_LIMIT, 1000);
    // Not 366: that is the unpaginated portfolio-history bound, and it is smaller.
    assert_eq!(nexus_exchange::rest::MAX_PORTFOLIO_HISTORY_LIMIT, 366);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param("limit", "1000"))
        .respond_with(page(serde_json::json!([]), None))
        .expect(1)
        .mount(&server)
        .await;

    authed(server.uri())
        .fetch_my_trades_paginated()
        .page_size(MAX_FILLS_LIMIT)
        .all()
        .await
        .unwrap();
}

#[tokio::test]
async fn page_size_over_the_maximum_fails_before_any_request() {
    // A request-schema violation costs no round trip — and on `/fills`, no
    // signature over a query the server would reject.
    let server = MockServer::start().await;

    for over in [MAX_FILLS_LIMIT + 1, 5000, 0] {
        let err = authed(server.uri())
            .fetch_my_trades_paginated()
            .page_size(over)
            .all()
            .await
            .expect_err("out-of-schema page size must be rejected");
        assert!(
            err.to_string().contains("fills page size must be between"),
            "unexpected error: {err}"
        );
    }

    let err = public(server.uri())
        .fetch_trades_paginated("BTC-USDX-PERP")
        .unwrap()
        .page_size(MAX_TRADES_LIMIT + 1)
        .all()
        .await
        .expect_err("out-of-schema page size must be rejected");
    // The message names the endpoint, because the maxima differ per endpoint.
    assert!(
        err.to_string().contains("trades page size must be between"),
        "unexpected error: {err}"
    );

    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_market_id_is_rejected_before_a_paginator_is_built() {
    let server = MockServer::start().await;
    let err = public(server.uri())
        .fetch_trades_paginated("")
        .expect_err("an empty market_id must not build a paginator");
    assert!(err.to_string().contains("market_id"), "unexpected: {err}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

// -- the flat `/fills` read now carries `limit` (ENG-8167) --------------------

/// The gap this closes: `fetch_my_trades` sent **no `limit` at all**, so a single
/// call could never read past the server's default of 100 fills, even though
/// v0.7.2 documents `limit` on `/fills` with `maximum: 1000`.
#[tokio::test]
async fn flat_fills_read_sends_the_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param("limit", "1000"))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(serde_json::json!([fill("f1")]), Some("cur-b")))
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri())
        .fetch_my_trades(Some(MAX_FILLS_LIMIT))
        .await
        .unwrap();

    // Still first-page-only: the cursor the server offered is not followed. That
    // is what keeps `Vec<Fill>` an honest return type and `_paginated` the way to
    // walk everything.
    assert_eq!(fills.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// `None` must send **no** `limit` parameter rather than a client-invented
/// default — the server owns the default (100 here), and substituting one would
/// silently change what an existing caller receives.
#[tokio::test]
async fn flat_fills_read_without_a_limit_sends_no_limit_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(FILLS_PATH))
        .and(query_param_is_missing("limit"))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([fill("f1")]), None))
        .expect(1)
        .mount(&server)
        .await;

    let fills = authed(server.uri()).fetch_my_trades(None).await.unwrap();
    assert_eq!(fills.len(), 1);
}

/// Validated against `/fills`'s own maximum before the request is **signed**, so
/// nothing the server would reject is ever put on the wire under a signature.
#[tokio::test]
async fn flat_fills_read_rejects_an_out_of_schema_limit_before_signing() {
    let server = MockServer::start().await;
    let client = authed(server.uri());

    for bad in [MAX_FILLS_LIMIT + 1, 5000, 0] {
        let err = client
            .fetch_my_trades(Some(bad))
            .await
            .expect_err("out-of-schema limit must be rejected");
        assert!(
            err.to_string().contains("fills page size must be between"),
            "unexpected error for {bad}: {err}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());

    // The bound is `/fills`'s own 1000, not the `366` of the unpaginated
    // `/account/portfolio-history`: a clamp there would reject valid requests.
    assert_eq!(MAX_FILLS_LIMIT, 1000);
    let (portfolio, fills_max) = (
        nexus_exchange::rest::MAX_PORTFOLIO_HISTORY_LIMIT,
        MAX_FILLS_LIMIT,
    );
    assert!(
        portfolio < fills_max,
        "{portfolio} must not be reused as the /fills bound ({fills_max})"
    );
}
