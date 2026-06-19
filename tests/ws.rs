//! Integration tests for the streaming WebSocket client, driven against a
//! local `tokio-tungstenite` server. They exercise the two reliability
//! features this client exists for: reconnect-with-backoff and the bounded,
//! gap-aware (order-preserving, drop-and-report) event channel.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nexus_exchange::ws::{Backoff, Event, Subscription};
use nexus_exchange::{Client, Config, Network};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A 32-byte hex secret, valid for HMAC signing of the token-mint request.
const TEST_SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// A client wired to a local ws:// server with a fast, deterministic backoff so
/// reconnect tests don't actually wait seconds.
fn client_for(addr: std::net::SocketAddr, capacity: usize) -> Client {
    let cfg = Config::with_base_url("http://unused")
        .with_ws_url(format!("ws://{addr}/ws"))
        .with_reconnect_backoff(
            Backoff::new()
                .with_initial(Duration::from_millis(5))
                .with_max(Duration::from_millis(20)),
        )
        .with_channel_capacity(capacity);
    Client::new(cfg)
}

/// Read the next text frame and return it as a JSON value (the subscribe frame).
async fn read_text(ws: &mut WebSocketStream<TcpStream>) -> Value {
    match ws.next().await {
        Some(Ok(Message::Text(t))) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn send_json(ws: &mut WebSocketStream<TcpStream>, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Await the next event with a hard upper bound, so a *missing* event (a
/// regression that drops a frame the test expects) fails with a named timeout
/// instead of hanging until the test harness kills the whole process. The bound
/// is generous relative to the millisecond-scale backoff used in these tests.
async fn next_event(sub: &mut Subscription) -> Option<Event> {
    match tokio::time::timeout(Duration::from_secs(5), sub.next()).await {
        Ok(event) => event,
        Err(_) => panic!("timed out waiting for the next event"),
    }
}

#[tokio::test]
async fn connects_subscribes_and_reconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Server: first connection echoes the subscribe frame, sends two messages,
    // then drops. Second connection (the reconnect) echoes the *resent*
    // subscribe frame and sends one more message, then waits for the client to
    // close — proving subscriptions are replayed after reconnect.
    let server = tokio::spawn(async move {
        // Connection 1.
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        let sub = read_text(&mut ws).await;
        send_json(&mut ws, json!({ "echo_sub": sub })).await;
        send_json(&mut ws, json!({ "n": 1 })).await;
        send_json(&mut ws, json!({ "n": 2 })).await;
        ws.close(None).await.unwrap();

        // Connection 2 (reconnect).
        let (sock2, _) = listener.accept().await.unwrap();
        let mut ws2 = tokio_tungstenite::accept_async(sock2).await.unwrap();
        let resub = read_text(&mut ws2).await;
        send_json(&mut ws2, json!({ "echo_sub": resub })).await;
        send_json(&mut ws2, json!({ "n": 3 })).await;
        // Hold open until the client sends Close.
        while let Some(Ok(msg)) = ws2.next().await {
            if msg.is_close() {
                break;
            }
        }
    });

    let client = client_for(addr, 64);
    let subscribe = json!({ "type": "subscribe", "channel": "trades" });
    let mut sub = client.connect(vec![subscribe.clone()]);

    // First lifecycle: Connected, echoed-sub, n=1, n=2.
    assert!(matches!(next_event(&mut sub).await, Some(Event::Connected)));
    assert_eq!(
        next_event(&mut sub).await.unwrap_message()["echo_sub"],
        subscribe
    );
    assert_eq!(next_event(&mut sub).await.unwrap_message()["n"], json!(1));
    assert_eq!(next_event(&mut sub).await.unwrap_message()["n"], json!(2));

    // The drop surfaces as Disconnected, then a fresh Connected.
    assert!(matches!(
        next_event(&mut sub).await,
        Some(Event::Disconnected(_))
    ));
    assert!(matches!(next_event(&mut sub).await, Some(Event::Connected)));

    // Reconnect replayed the subscription frame.
    assert_eq!(
        next_event(&mut sub).await.unwrap_message()["echo_sub"],
        subscribe
    );
    assert_eq!(next_event(&mut sub).await.unwrap_message()["n"], json!(3));

    sub.close().await;
    server.await.unwrap();
}

#[tokio::test]
async fn backpressure_preserves_order_and_reports_gaps() {
    const BURST: usize = 200;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        let _sub = read_text(&mut ws).await;
        // Fire a tight burst far exceeding the channel capacity, then close so
        // the consumer has a clean end-of-connection signal.
        for i in 0..BURST {
            send_json(&mut ws, json!({ "seq": i })).await;
        }
        ws.close(None).await.unwrap();
    });

    // A small capacity exposes the consumer to backpressure under a tight burst.
    let client = client_for(addr, 4);
    let mut sub = client.connect(vec![json!({ "type": "subscribe" })]);

    assert!(matches!(next_event(&mut sub).await, Some(Event::Connected)));

    // Walk events until the connection drops. Delivery is order-preserving and
    // gap-aware: each frame's seq equals the running count of (delivered +
    // reported-dropped), so a reorder or duplicate would trip the assert, and
    // any frames dropped under backpressure are accounted for by a `Lagged`.
    let mut expected: u64 = 0;
    let mut delivered: u64 = 0;
    let mut lagged: u64 = 0;
    loop {
        match next_event(&mut sub).await {
            Some(Event::Message(v)) => {
                let seq = v["seq"].as_u64().unwrap();
                assert_eq!(
                    seq, expected,
                    "reorder/duplicate: got {seq}, expected {expected}"
                );
                expected += 1;
                delivered += 1;
            }
            Some(Event::Lagged { dropped }) => {
                assert!(dropped > 0);
                expected += dropped;
                lagged += dropped;
            }
            Some(Event::Connected) => panic!("unexpected reconnect during the burst"),
            // Server closed the socket: end of this connection.
            Some(Event::Disconnected(_)) | None => break,
            Some(_) => {}
        }
    }

    // The first frames arrived in order, and nothing observed exceeds the burst.
    // (Trailing drops after the last delivered frame go unreported — no later
    // message flushes them — so this is `<=`, not `==`; exact accounting within
    // a connection is proven by the `deliver` unit test.)
    assert!(delivered >= 1, "expected at least one delivered frame");
    assert!(delivered + lagged <= BURST as u64);

    sub.close().await;
    let _ = server.await;
}

#[tokio::test]
async fn connect_failure_surfaces_disconnected_and_keeps_retrying() {
    // Bind then drop the listener so the port is closed: every connect refuses.
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };

    let client = client_for(addr, 8);
    let mut sub = client.connect(vec![]);

    // The client reports the failure rather than panicking, and (because it
    // keeps retrying against a dead port) keeps reporting it.
    for _ in 0..3 {
        match next_event(&mut sub).await {
            Some(Event::Disconnected(reason)) => assert!(reason.contains("connect failed")),
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    sub.close().await;
}

/// `connect_ws` mints a single-use token over REST, then presents it on the
/// WebSocket upgrade as a `token=` query parameter — so a caller never has to
/// know the indexer's WS host *or* wire the mint up by hand.
// The handshake callback's `Err` variant (tungstenite's `ErrorResponse`) is
// large, but its shape is fixed by the `Callback` trait and we only ever
// return `Ok`, so the lint doesn't apply here.
#[allow(clippy::result_large_err)]
#[tokio::test]
async fn connect_ws_mints_token_and_presents_it_on_upgrade() {
    // REST: the signed token mint (`POST /ws/token`).
    let rest = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ws/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "token": "tok-abc" })))
        .mount(&rest)
        .await;

    // WS: capture the upgrade request's query string, then behave normally.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (qtx, qrx) = tokio::sync::oneshot::channel::<Option<String>>();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let capture = move |req: &Request, resp: Response| {
            let _ = qtx.send(req.uri().query().map(str::to_string));
            Ok(resp)
        };
        let mut ws = tokio_tungstenite::accept_hdr_async(sock, capture)
            .await
            .unwrap();
        send_json(&mut ws, json!({ "hello": true })).await;
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_close() {
                break;
            }
        }
    });

    let cfg = Config::with_base_url(rest.uri())
        .with_ws_url(format!("ws://{addr}/ws"))
        .api_key("nx", TEST_SECRET)
        .with_reconnect_backoff(
            Backoff::new()
                .with_initial(Duration::from_millis(5))
                .with_max(Duration::from_millis(20)),
        );
    let client = Client::new(cfg);

    let mut sub = client.connect_ws(vec![]).await.expect("mint + connect");
    assert!(matches!(next_event(&mut sub).await, Some(Event::Connected)));
    assert_eq!(
        next_event(&mut sub).await.unwrap_message()["hello"],
        json!(true)
    );

    let query = tokio::time::timeout(Duration::from_secs(5), qrx)
        .await
        .expect("upgrade captured")
        .unwrap();
    assert_eq!(query.as_deref(), Some("token=tok-abc"));

    sub.close().await;
    let _ = server.await;
}

/// `connect_ws` refuses — fast, before any network round-trip — when the
/// network has no confirmed WS host (ENG-3398). No server is mocked, so a mint
/// attempt would hang/fail the test; the check must short-circuit before it.
#[tokio::test]
async fn connect_ws_errors_when_endpoint_unconfigured() {
    let client = Client::new(Config::new(Network::Stable).api_key("nx", TEST_SECRET));
    let err = client.connect_ws(vec![]).await.unwrap_err();
    assert!(
        err.to_string().contains("no WebSocket endpoint configured"),
        "unexpected error: {err}"
    );
}

/// The low-level `connect` on a network with no WS host reports the missing
/// endpoint once and then ends the stream, instead of spinning a reconnect
/// loop against a host that can't exist.
#[tokio::test]
async fn connect_without_endpoint_reports_once_then_ends() {
    let client = Client::new(Config::new(Network::Stable));
    let mut sub = client.connect(vec![]);
    match next_event(&mut sub).await {
        Some(Event::Disconnected(reason)) => {
            assert!(
                reason.contains("no WebSocket endpoint configured"),
                "{reason}"
            )
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
    assert!(
        next_event(&mut sub).await.is_none(),
        "stream should end after reporting the missing endpoint"
    );
}

/// Test-only helper to unwrap a `Message` event.
trait EventExt {
    fn unwrap_message(self) -> Value;
}

impl EventExt for Option<Event> {
    fn unwrap_message(self) -> Value {
        match self {
            Some(Event::Message(v)) => v,
            other => panic!("expected Message event, got {other:?}"),
        }
    }
}
