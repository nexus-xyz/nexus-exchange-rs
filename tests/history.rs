//! The three history endpoints spec v0.7.2 added, end-to-end through `Client`:
//! `GET /api/v1/orders/history`, `GET /api/v1/positions/closed` and
//! `GET /api/v1/account/equity-history`.
//!
//! Every test here starts from a **`Client` method** against a mock server, so it
//! fails if the endpoint is not reachable from the public API, if `limit` /
//! `cursor` do not reach the wire, or if `X-Next-Cursor` is not read. (The lesson
//! from ENG-8084: `Paginator` had a green unit-test suite for months while nothing
//! on `Client` returned one. Tests that never touch a `Client` method prove
//! nothing about reachability.)
//!
//! The other thing pinned here is that the `limit` maxima are **per endpoint**:
//! 500 / 200 / 720, not one shared bound, and emphatically not the `366` that
//! belongs to the un-paginated `/account/portfolio-history`.

use nexus_exchange::rest::{
    MAX_CLOSED_POSITIONS_LIMIT, MAX_EQUITY_HISTORY_LIMIT, MAX_ORDER_HISTORY_LIMIT,
    MAX_PORTFOLIO_HISTORY_LIMIT,
};
use nexus_exchange::types::Side;
use nexus_exchange::{Client, Config};
use rust_decimal::Decimal;
use wiremock::matchers::{header_exists, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ORDER_HISTORY_PATH: &str = "/api/v1/orders/history";
const CLOSED_POSITIONS_PATH: &str = "/api/v1/positions/closed";
const EQUITY_HISTORY_PATH: &str = "/api/v1/account/equity-history";

#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
fn authed(uri: String) -> Client {
    Client::new(Config::with_base_url(uri).api_key(
        "nx_test",
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    ))
}

fn order_entry(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "market_id": "BTC-USDX-PERP", "side": "buy", "order_type": "limit",
        "price": "50000.5", "size": "2", "filled_qty": "2", "status": "Filled",
        "cancellation_reason": null, "created_at_ms": 1776033900000i64,
        "completed_at_ms": 1776033901000i64
    })
}

fn closed_position(market: &str) -> serde_json::Value {
    serde_json::json!({
        "market_id": market, "side": "Long", "size": "1.5", "entry_price": "49000.25",
        "exit_price": "51000.75", "realized_pnl": "3000.75",
        "closed_at_ms": 1776033900000i64
    })
}

fn equity_point(ts: i64) -> serde_json::Value {
    serde_json::json!({ "timestamp_ms": ts, "equity": 12345.5 })
}

/// A page response: the body, plus `X-Next-Cursor` when there is a next page.
fn page(body: serde_json::Value, next_cursor: Option<&str>) -> ResponseTemplate {
    let template = ResponseTemplate::new(200).set_body_json(body);
    match next_cursor {
        Some(cursor) => template.insert_header("x-next-cursor", cursor),
        None => template,
    }
}

// -- reachable from `Client`, and they page ----------------------------------

#[tokio::test]
async fn order_history_paginator_walks_every_page_and_signs_each() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(
            serde_json::json!([order_entry("o1"), order_entry("o2")]),
            Some("oh-2"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    // The cursor rides in the query, so page 2 is signed over a *different*
    // canonical string — each page is independently signed.
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .and(query_param("cursor", "oh-2"))
        .and(header_exists("x-signature"))
        .respond_with(page(serde_json::json!([order_entry("o3")]), None))
        .expect(1)
        .mount(&server)
        .await;

    let orders = authed(server.uri())
        .fetch_order_history_paginated()
        .all()
        .await
        .unwrap();

    let ids: Vec<&str> = orders
        .iter()
        .map(|o| o.id.as_deref().expect("id present"))
        .collect();
    assert_eq!(ids, vec!["o1", "o2", "o3"]);
    // Exactly two requests: no speculative fetch past the final page.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn closed_positions_paginator_walks_every_page_and_signs_each() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(
            serde_json::json!([closed_position("BTC-USDX-PERP")]),
            Some("cp-2"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .and(query_param("cursor", "cp-2"))
        .and(header_exists("x-signature"))
        .respond_with(page(
            serde_json::json!([closed_position("ETH-USDX-PERP")]),
            None,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let closed = authed(server.uri())
        .fetch_closed_positions_paginated()
        .all()
        .await
        .unwrap();

    let markets: Vec<&str> = closed
        .iter()
        .map(|p| p.market_id.as_deref().expect("market_id present"))
        .collect();
    assert_eq!(markets, vec!["BTC-USDX-PERP", "ETH-USDX-PERP"]);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn equity_history_paginator_walks_every_page_and_signs_each() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(EQUITY_HISTORY_PATH))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(
            serde_json::json!([equity_point(1776033900000), equity_point(1776033905000)]),
            Some("eq-2"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EQUITY_HISTORY_PATH))
        .and(query_param("cursor", "eq-2"))
        .and(header_exists("x-signature"))
        .respond_with(page(serde_json::json!([equity_point(1776033910000)]), None))
        .expect(1)
        .mount(&server)
        .await;

    let points = authed(server.uri())
        .fetch_equity_history_paginated()
        .all()
        .await
        .unwrap();

    let stamps: Vec<i64> = points
        .iter()
        .map(|p| p.timestamp_ms.expect("timestamp present"))
        .collect();
    assert_eq!(
        stamps,
        vec![1776033900000i64, 1776033905000, 1776033910000],
        "samples must stay in server order (oldest first)"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

/// Manual paging: `next_page` hands back the cursor a resumable job persists, and
/// `starting_after` puts it back on the wire.
#[tokio::test]
async fn order_history_manual_paging_round_trips_the_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([order_entry("o1")]), Some("oh-2")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .and(query_param("cursor", "oh-2"))
        .respond_with(page(serde_json::json!([order_entry("o2")]), None))
        .mount(&server)
        .await;

    let client = authed(server.uri());
    let mut pager = client.fetch_order_history_paginated();
    let first = pager.next_page().await.unwrap().unwrap();
    assert!(!first.is_last());
    let saved = first
        .next_cursor
        .clone()
        .expect("cursor on a non-final page");
    assert_eq!(saved.as_str(), "oh-2");

    // A fresh paginator resuming from the persisted cursor must skip page 1.
    let resumed = client
        .fetch_order_history_paginated()
        .starting_after(saved)
        .all()
        .await
        .unwrap();
    let ids: Vec<&str> = resumed.iter().map(|o| o.id.as_deref().unwrap()).collect();
    assert_eq!(ids, vec!["o2"], "resumed walk must start after the cursor");
}

// -- the flat first-page methods ---------------------------------------------

#[tokio::test]
async fn flat_order_history_sends_limit_and_no_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .and(query_param("limit", "500"))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(serde_json::json!([order_entry("o1")]), Some("oh-2")))
        .expect(1)
        .mount(&server)
        .await;

    let orders = authed(server.uri())
        .fetch_order_history(Some(MAX_ORDER_HISTORY_LIMIT))
        .await
        .unwrap();

    // First page only: the cursor the server offered is not followed.
    assert_eq!(orders.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn flat_closed_positions_sends_limit_and_no_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .and(query_param("limit", "200"))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(
            serde_json::json!([closed_position("BTC-USDX-PERP")]),
            Some("cp-2"),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let closed = authed(server.uri())
        .fetch_closed_positions(Some(MAX_CLOSED_POSITIONS_LIMIT))
        .await
        .unwrap();

    assert_eq!(closed.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// `None` must send **no** `limit` at all, not a client-invented default: on this
/// endpoint the server's own default is 720, so substituting anything smaller
/// would silently truncate the window.
#[tokio::test]
async fn flat_equity_history_without_a_limit_sends_no_limit_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(EQUITY_HISTORY_PATH))
        .and(query_param_is_missing("limit"))
        .and(query_param_is_missing("cursor"))
        .and(header_exists("x-signature"))
        .respond_with(page(serde_json::json!([equity_point(1776033900000)]), None))
        .expect(1)
        .mount(&server)
        .await;

    let points = authed(server.uri())
        .fetch_equity_history(None)
        .await
        .unwrap();
    assert_eq!(points.len(), 1);
}

// -- `limit` maxima are per endpoint ----------------------------------------

/// The asymmetry, stated as one test: a size valid on one endpoint is out of range
/// on another. A single shared bound cannot satisfy this table.
#[tokio::test]
async fn limit_maxima_are_per_endpoint_and_not_interchangeable() {
    let server = MockServer::start().await;
    let client = authed(server.uri());

    // /orders/history: 501 is out of range (500 in range is pinned by
    // `flat_order_history_sends_limit_and_no_cursor`, which sends it).
    let err = client
        .fetch_order_history(Some(MAX_ORDER_HISTORY_LIMIT + 1))
        .await
        .expect_err("501 is out of range on /orders/history");
    assert!(
        err.to_string().contains("orders/history page size"),
        "error must name the endpoint: {err}"
    );

    // 500 is *valid* on /orders/history but out of range on /positions/closed.
    let err = client
        .fetch_closed_positions(Some(MAX_ORDER_HISTORY_LIMIT))
        .await
        .expect_err("500 is out of range on /positions/closed (max 200)");
    assert!(
        err.to_string().contains("positions/closed page size"),
        "error must name the endpoint: {err}"
    );

    // And 720 is valid on /account/equity-history but out of range on both others.
    let err = client
        .fetch_order_history(Some(MAX_EQUITY_HISTORY_LIMIT))
        .await
        .expect_err("720 is out of range on /orders/history (max 500)");
    assert!(err.to_string().contains("orders/history page size"));

    // Zero is rejected everywhere: it would return an empty page, which on a
    // cursor-paginated endpoint reads as "no more results" and ends a walk at
    // zero items.
    for endpoint in [
        "orders/history",
        "positions/closed",
        "account/equity-history",
    ] {
        let err = match endpoint {
            "orders/history" => client.fetch_order_history(Some(0)).await.unwrap_err(),
            "positions/closed" => client.fetch_closed_positions(Some(0)).await.unwrap_err(),
            _ => client.fetch_equity_history(Some(0)).await.unwrap_err(),
        };
        assert!(err.to_string().contains(endpoint), "{endpoint}: {err}");
    }

    // Nothing was signed or sent for any rejected value.
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// `366` is `/account/portfolio-history`'s bound and belongs to no paginated
/// endpoint. Pinning it here because a shared clamp at 366 would sit *below*
/// equity-history's own default of 720 and reject a plain default request
/// client-side.
#[tokio::test]
async fn portfolio_history_limit_is_not_a_paginated_bound() {
    assert_eq!(MAX_PORTFOLIO_HISTORY_LIMIT, 366);
    assert_eq!(MAX_EQUITY_HISTORY_LIMIT, 720);
    // Bound through locals so this is a real comparison, not a const-folded
    // `assert!(true)`: 366 sits *below* equity-history's own default of 720, which
    // is exactly why it cannot be reused as a shared paginated clamp.
    let (portfolio, equity) = (MAX_PORTFOLIO_HISTORY_LIMIT, MAX_EQUITY_HISTORY_LIMIT);
    assert!(
        portfolio < equity,
        "a shared clamp at {portfolio} would reject equity-history's default of {equity}"
    );
    assert_eq!(MAX_ORDER_HISTORY_LIMIT, 500);
    assert_eq!(MAX_CLOSED_POSITIONS_LIMIT, 200);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(EQUITY_HISTORY_PATH))
        .and(query_param("limit", "720"))
        .respond_with(page(serde_json::json!([equity_point(1776033900000)]), None))
        .expect(1)
        .mount(&server)
        .await;

    // 720 > 366 and must be accepted, not clamped.
    authed(server.uri())
        .fetch_equity_history(Some(MAX_EQUITY_HISTORY_LIMIT))
        .await
        .expect("the endpoint's own maximum must be accepted");
}

/// The paginator validates `page_size` too — on the first page fetch, since
/// `page_size` is an infallible builder.
#[tokio::test]
async fn paginator_page_size_is_validated_before_the_first_request() {
    let server = MockServer::start().await;
    let err = authed(server.uri())
        .fetch_closed_positions_paginated()
        .page_size(MAX_CLOSED_POSITIONS_LIMIT + 1)
        .next_page()
        .await
        .expect_err("out-of-schema page size must be rejected");
    assert!(
        err.to_string().contains("positions/closed page size"),
        "unexpected: {err}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());

    // At the maximum it is sent.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .and(query_param("limit", "200"))
        .respond_with(page(
            serde_json::json!([closed_position("BTC-USDX-PERP")]),
            None,
        ))
        .expect(1)
        .mount(&server)
        .await;
    authed(server.uri())
        .fetch_closed_positions_paginated()
        .page_size(MAX_CLOSED_POSITIONS_LIMIT)
        .all()
        .await
        .unwrap();
}

// -- termination ------------------------------------------------------------

/// No `X-Next-Cursor` ⇒ last page. Not an error, not a retry, no extra request.
/// Checked on all three endpoints, since each has its own paginator wiring.
#[tokio::test]
async fn absent_cursor_header_ends_the_walk_on_every_endpoint() {
    for (path_str, body) in [
        (ORDER_HISTORY_PATH, serde_json::json!([order_entry("o1")])),
        (
            CLOSED_POSITIONS_PATH,
            serde_json::json!([closed_position("BTC-USDX-PERP")]),
        ),
        (
            EQUITY_HISTORY_PATH,
            serde_json::json!([equity_point(1776033900000)]),
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(path_str))
            .respond_with(page(body, None))
            .expect(1)
            .mount(&server)
            .await;

        let client = authed(server.uri());
        let count = match path_str {
            ORDER_HISTORY_PATH => client
                .fetch_order_history_paginated()
                .all()
                .await
                .unwrap()
                .len(),
            CLOSED_POSITIONS_PATH => client
                .fetch_closed_positions_paginated()
                .all()
                .await
                .unwrap()
                .len(),
            _ => client
                .fetch_equity_history_paginated()
                .all()
                .await
                .unwrap()
                .len(),
        };
        assert_eq!(count, 1, "{path_str}");
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "{path_str}: exactly one request"
        );
    }
}

/// An empty page that still carries a cursor is **not** the end — a sparse window
/// must not truncate the walk.
#[tokio::test]
async fn empty_page_with_a_cursor_keeps_paging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .and(query_param_is_missing("cursor"))
        .respond_with(page(serde_json::json!([]), Some("cp-2")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .and(query_param("cursor", "cp-2"))
        .respond_with(page(
            serde_json::json!([closed_position("BTC-USDX-PERP")]),
            None,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let closed = authed(server.uri())
        .fetch_closed_positions_paginated()
        .all()
        .await
        .unwrap();
    assert_eq!(
        closed.len(),
        1,
        "the sparse first page must not end the walk"
    );
}

/// A present-but-blank header counts as absent: an empty cursor cannot be sent
/// back, and passing it on would re-request the first page forever.
#[tokio::test]
async fn blank_cursor_header_is_treated_as_absent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(EQUITY_HISTORY_PATH))
        .respond_with(page(
            serde_json::json!([equity_point(1776033900000)]),
            Some("   "),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let points = authed(server.uri())
        .fetch_equity_history_paginated()
        .all()
        .await
        .unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// A server echoing the cursor it was given cannot advance, so the walk stops
/// rather than spinning. It stops **observably**: the final page still carries a
/// non-`None` `next_cursor`, which is how this SDK signals the stall instead of
/// adding an error to the paginator contract (a deliberate divergence from
/// `nexus-exchange-py`, which raises `PaginationError` — see ENG-8084 / PR #112).
///
/// Only two stuck responses are registered, so a regression that drops the guard
/// fails loudly on an unmatched third request rather than hanging the suite.
#[tokio::test]
async fn repeated_cursor_stops_the_walk_observably() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .respond_with(page(serde_json::json!([order_entry("o1")]), Some("stuck")))
        .expect(2)
        .mount(&server)
        .await;

    let mut pager = authed(server.uri()).fetch_order_history_paginated();
    let first = pager.next_page().await.unwrap().unwrap();
    assert_eq!(first.next_cursor.as_ref().unwrap().as_str(), "stuck");
    let second = pager.next_page().await.unwrap().unwrap();
    // The stall is visible: a "last" page that still advertises a next cursor.
    assert_eq!(second.next_cursor.as_ref().unwrap().as_str(), "stuck");
    assert!(pager.next_page().await.unwrap().is_none(), "must not spin");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

// -- decoding: absent is never zero ----------------------------------------

/// A full payload decodes into exact decimal values, and the money fields are
/// decimal *strings* on the wire.
#[tokio::test]
async fn order_history_decodes_money_as_exact_decimals() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .respond_with(page(serde_json::json!([order_entry("o1")]), None))
        .mount(&server)
        .await;

    let orders = authed(server.uri())
        .fetch_order_history(None)
        .await
        .unwrap();
    let o = &orders[0];
    assert_eq!(o.side, Some(Side::Buy));
    assert_eq!(o.order_type.as_deref(), Some("limit"));
    assert_eq!(o.status.as_deref(), Some("Filled"));
    assert_eq!(o.price, Some("50000.5".parse::<Decimal>().unwrap()));
    assert_eq!(o.size, Some(Decimal::from(2)));
    assert_eq!(o.filled_qty, Some(Decimal::from(2)));
    assert_eq!(o.created_at_ms, Some(1776033900000));
    assert_eq!(o.completed_at_ms, Some(1776033901000));
    // Spec-nullable, and explicitly null here.
    assert_eq!(o.cancellation_reason, None);
}

/// The spec marks no field of these three schemas `required`, so a slim payload
/// must decode — and every absent field must read as `None`, never as a
/// fabricated `0` / `""` / epoch timestamp. `price` is additionally spec-nullable
/// (market orders carry no limit price) and `null` must not become `0`, which
/// would read as a real price of zero.
#[tokio::test]
async fn absent_and_null_fields_decode_as_none_not_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORDER_HISTORY_PATH))
        .respond_with(page(
            // A market order: `price` explicitly null, everything else omitted.
            serde_json::json!([{ "id": "o1", "price": null }]),
            None,
        ))
        .mount(&server)
        .await;

    let orders = authed(server.uri())
        .fetch_order_history(None)
        .await
        .unwrap();
    let o = &orders[0];
    assert_eq!(o.id.as_deref(), Some("o1"));
    assert_eq!(o.price, None, "a null limit price must not become 0");
    assert_eq!(o.size, None, "an absent size must not become 0");
    assert_eq!(
        o.filled_qty, None,
        "an absent filled_qty must not read as 'nothing filled'"
    );
    assert_eq!(
        o.status, None,
        "an absent status must not read as the empty string"
    );
    assert_eq!(o.market_id, None);
    assert_eq!(o.side, None);
    assert_eq!(
        o.created_at_ms, None,
        "an absent timestamp must not read as the Unix epoch"
    );
    assert_eq!(o.completed_at_ms, None);
}

#[tokio::test]
async fn closed_position_slim_payload_decodes_without_fabricating_pnl() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .respond_with(page(
            serde_json::json!([{ "market_id": "BTC-USDX-PERP" }]),
            None,
        ))
        .mount(&server)
        .await;

    let closed = authed(server.uri())
        .fetch_closed_positions(None)
        .await
        .unwrap();
    let p = &closed[0];
    assert_eq!(p.market_id.as_deref(), Some("BTC-USDX-PERP"));
    assert_eq!(
        p.realized_pnl, None,
        "an absent realized_pnl must not report a losing close as break-even"
    );
    assert_eq!(p.exit_price, None);
    assert_eq!(p.entry_price, None);
    assert_eq!(p.size, None);
    assert_eq!(p.side, None);
    assert_eq!(p.closed_at_ms, None);
}

/// A full closed position decodes exactly, including a **negative** realized PnL —
/// the case a defaulted zero would hide.
#[tokio::test]
async fn closed_position_decodes_a_negative_realized_pnl() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLOSED_POSITIONS_PATH))
        .respond_with(page(
            serde_json::json!([{
                "market_id": "BTC-USDX-PERP", "side": "Short", "size": "0.5",
                "entry_price": "49000.25", "exit_price": "51000.75",
                "realized_pnl": "-1000.25", "closed_at_ms": 1776033900000i64
            }]),
            None,
        ))
        .mount(&server)
        .await;

    let closed = authed(server.uri())
        .fetch_closed_positions(None)
        .await
        .unwrap();
    let p = &closed[0];
    assert_eq!(p.side.as_deref(), Some("Short"));
    assert_eq!(p.realized_pnl, Some("-1000.25".parse::<Decimal>().unwrap()));
    assert_eq!(p.entry_price, Some("49000.25".parse::<Decimal>().unwrap()));
    assert_eq!(p.exit_price, Some("51000.75".parse::<Decimal>().unwrap()));
    assert_eq!(p.closed_at_ms, Some(1776033900000));
}

/// `EquityPoint.equity` is a JSON **number** in the spec (unlike
/// `PortfolioPoint.equity`, a decimal string derived from the same value), so it
/// decodes through the `float` adapter. An absent sample field still reads as
/// `None`, not a wiped-out account.
#[tokio::test]
async fn equity_point_decodes_a_json_number_and_never_fabricates_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(EQUITY_HISTORY_PATH))
        .respond_with(page(
            serde_json::json!([
                { "timestamp_ms": 1776033900000i64, "equity": 12345.5 },
                {},
            ]),
            None,
        ))
        .mount(&server)
        .await;

    let points = authed(server.uri())
        .fetch_equity_history(None)
        .await
        .unwrap();
    assert_eq!(points[0].timestamp_ms, Some(1776033900000));
    assert_eq!(
        points[0].equity,
        Some("12345.5".parse::<Decimal>().unwrap())
    );
    assert_eq!(
        points[1].equity, None,
        "an absent equity sample must not read as a zero balance"
    );
    assert_eq!(points[1].timestamp_ms, None);
}
