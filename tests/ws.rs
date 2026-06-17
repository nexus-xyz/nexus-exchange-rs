//! Integration tests for the streaming WebSocket client, driven against a
//! local `tokio-tungstenite` server. They exercise the two reliability
//! features this client exists for: reconnect-with-backoff and the bounded,
//! lossless (backpressured) event channel.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nexus_exchange::ws::{Backoff, Event};
use nexus_exchange::{Client, Config};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

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

/// Pull events until `n` `Message` events have been collected, returning them.
/// Panics on early stream end. Other event variants are recorded separately.
async fn collect_messages(
    sub: &mut nexus_exchange::ws::Subscription,
    n: usize,
) -> (Vec<Value>, Vec<Event>) {
    let mut messages = Vec::new();
    let mut others = Vec::new();
    while messages.len() < n {
        match sub.next().await {
            Some(Event::Message(v)) => messages.push(v),
            Some(other) => others.push(other),
            None => panic!("stream ended after {} of {n} messages", messages.len()),
        }
    }
    (messages, others)
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
    assert!(matches!(sub.next().await, Some(Event::Connected)));
    assert_eq!(sub.next().await.unwrap_message()["echo_sub"], subscribe);
    assert_eq!(sub.next().await.unwrap_message()["n"], json!(1));
    assert_eq!(sub.next().await.unwrap_message()["n"], json!(2));

    // The drop surfaces as Disconnected, then a fresh Connected.
    assert!(matches!(sub.next().await, Some(Event::Disconnected(_))));
    assert!(matches!(sub.next().await, Some(Event::Connected)));

    // Reconnect replayed the subscription frame.
    assert_eq!(sub.next().await.unwrap_message()["echo_sub"], subscribe);
    assert_eq!(sub.next().await.unwrap_message()["n"], json!(3));

    sub.close().await;
    server.await.unwrap();
}

#[tokio::test]
async fn bounded_channel_is_lossless_under_backpressure() {
    const BURST: usize = 200;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        let _sub = read_text(&mut ws).await;
        // Fire a tight burst far exceeding the channel capacity.
        for i in 0..BURST {
            send_json(&mut ws, json!({ "seq": i })).await;
        }
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_close() {
                break;
            }
        }
    });

    // Capacity of 1 forces the read loop to block on nearly every frame.
    let client = client_for(addr, 1);
    let mut sub = client.connect(vec![json!({ "type": "subscribe" })]);

    assert!(matches!(sub.next().await, Some(Event::Connected)));

    // A deliberately slow consumer must still see every frame, in order — the
    // bound applies backpressure rather than dropping.
    let (messages, others) = collect_messages(&mut sub, BURST).await;
    assert!(
        others.is_empty(),
        "unexpected lifecycle events during burst: {others:?}"
    );
    let seqs: Vec<u64> = messages
        .iter()
        .map(|m| m["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, (0..BURST as u64).collect::<Vec<_>>());

    sub.close().await;
    server.await.unwrap();
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
        match sub.next().await {
            Some(Event::Disconnected(reason)) => assert!(reason.contains("connect failed")),
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    sub.close().await;
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
