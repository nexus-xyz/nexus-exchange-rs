//! Integration tests for the typed, protocol-aware streaming client
//! ([`Client::subscribe`] / [`MessageStream`]), driven against a local
//! `tokio-tungstenite` server. They exercise the two things this layer adds over
//! the raw client: op-envelope decoding and **cursor-based resume** — on
//! reconnect the client must replay each `subscribe` with a `since` cursor equal
//! to the last `seq` it processed.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nexus_exchange::ws::{Channel, MessageStream, ServerMessage};
use nexus_exchange::{Client, Config, Error};
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
            nexus_exchange::ws::Backoff::new()
                .with_initial(Duration::from_millis(5))
                .with_max(Duration::from_millis(20)),
        )
        .with_channel_capacity(capacity);
    Client::new(cfg)
}

async fn read_text(ws: &mut WebSocketStream<TcpStream>) -> Value {
    match ws.next().await {
        Some(Ok(Message::Text(t))) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn send_json(ws: &mut WebSocketStream<TcpStream>, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Await the next stream item with a hard upper bound, so a missing item fails
/// with a named timeout instead of hanging the whole harness.
async fn next_item(stream: &mut MessageStream) -> Option<Result<ServerMessage, Error>> {
    match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
        Ok(item) => item,
        Err(_) => panic!("timed out waiting for the next stream item"),
    }
}

#[tokio::test]
async fn decodes_frames_and_resumes_from_cursor_on_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Server:
    //  Connection 1 — assert the initial subscribe carries NO `since` (fresh),
    //  ack with seq_at_join=10, send updates seq 11 & 12, then drop.
    //  Connection 2 (reconnect) — assert the replayed subscribe carries
    //  `since: 12` (the last seq processed), then send update seq 13 and hold
    //  open until the client closes.
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        let sub = read_text(&mut ws).await;
        assert_eq!(sub["op"], "subscribe");
        assert_eq!(sub["channel"], "trades");
        assert_eq!(sub["market"], "BTC-USDX-PERP");
        assert!(
            sub.get("since").is_none(),
            "first subscribe must have no cursor"
        );

        send_json(
            &mut ws,
            json!({ "op": "subscribed", "channel": "trades", "market": "BTC-USDX-PERP", "seq_at_join": 10 }),
        )
        .await;
        send_json(
            &mut ws,
            json!({ "op": "event", "channel": "trades", "market": "BTC-USDX-PERP", "seq": 11, "payload": { "px": "1" } }),
        )
        .await;
        send_json(
            &mut ws,
            json!({ "op": "event", "channel": "trades", "market": "BTC-USDX-PERP", "seq": 12, "payload": { "px": "2" } }),
        )
        .await;
        ws.close(None).await.unwrap();

        // Reconnect: the resubscribe must resume after seq 12.
        let (sock2, _) = listener.accept().await.unwrap();
        let mut ws2 = tokio_tungstenite::accept_async(sock2).await.unwrap();
        let resub = read_text(&mut ws2).await;
        assert_eq!(resub["op"], "subscribe");
        assert_eq!(
            resub["since"],
            json!(12),
            "reconnect must resume from cursor"
        );
        send_json(
            &mut ws2,
            json!({ "op": "event", "channel": "trades", "market": "BTC-USDX-PERP", "seq": 13, "payload": { "px": "3" } }),
        )
        .await;
        while let Some(Ok(msg)) = ws2.next().await {
            if msg.is_close() {
                break;
            }
        }
    });

    let client = client_for(addr, 64);
    let mut stream = client
        .subscribe(vec![Channel::trades("BTC-USDX-PERP")])
        .unwrap();

    // subscribed (seq_at_join=10), then updates 11 and 12.
    match next_item(&mut stream).await {
        Some(Ok(ServerMessage::Subscribed { seq_at_join, .. })) => assert_eq!(seq_at_join, 10),
        other => panic!("expected Subscribed, got {other:?}"),
    }
    for expected in [11, 12] {
        match next_item(&mut stream).await {
            Some(Ok(ServerMessage::Event { seq, .. })) => assert_eq!(seq, expected),
            other => panic!("expected Update {expected}, got {other:?}"),
        }
    }

    // After the transparent reconnect, the resumed stream delivers seq 13.
    match next_item(&mut stream).await {
        Some(Ok(ServerMessage::Event { seq, .. })) => assert_eq!(seq, 13),
        other => panic!("expected resumed Update 13, got {other:?}"),
    }

    stream.close().await;
    server.await.unwrap();
}

#[tokio::test]
async fn out_of_sync_is_surfaced_and_clears_the_resume_cursor() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Server:
    //  Connection 1 — ack with seq_at_join=10 (seeding the cursor), then send
    //  `out_of_sync`: the client's resume point is now unsatisfiable, so it must
    //  surface the frame and drop the cursor. Then drop the socket.
    //  Connection 2 (reconnect) — the replayed subscribe must carry NO `since`
    //  (the cursor was cleared, not held at 10), then deliver seq 20.
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        let _sub = read_text(&mut ws).await;
        send_json(
            &mut ws,
            json!({ "op": "subscribed", "channel": "trades", "market": "BTC-USDX-PERP", "seq_at_join": 10 }),
        )
        .await;
        send_json(
            &mut ws,
            json!({ "op": "out_of_sync", "channel": "trades", "market": "BTC-USDX-PERP", "oldest_seq": 50 }),
        )
        .await;
        ws.close(None).await.unwrap();

        let (sock2, _) = listener.accept().await.unwrap();
        let mut ws2 = tokio_tungstenite::accept_async(sock2).await.unwrap();
        let resub = read_text(&mut ws2).await;
        assert_eq!(resub["op"], "subscribe");
        assert!(
            resub.get("since").is_none(),
            "out_of_sync must clear the cursor, so the resubscribe has no `since`"
        );
        send_json(
            &mut ws2,
            json!({ "op": "event", "channel": "trades", "market": "BTC-USDX-PERP", "seq": 20, "payload": {} }),
        )
        .await;
        while let Some(Ok(msg)) = ws2.next().await {
            if msg.is_close() {
                break;
            }
        }
    });

    let client = client_for(addr, 64);
    let mut stream = client
        .subscribe(vec![Channel::trades("BTC-USDX-PERP")])
        .unwrap();

    match next_item(&mut stream).await {
        Some(Ok(ServerMessage::Subscribed { seq_at_join, .. })) => assert_eq!(seq_at_join, 10),
        other => panic!("expected Subscribed, got {other:?}"),
    }
    match next_item(&mut stream).await {
        Some(Ok(ServerMessage::OutOfSync {
            oldest_seq, market, ..
        })) => {
            assert_eq!(oldest_seq, Some(50));
            assert_eq!(market.as_deref(), Some("BTC-USDX-PERP"));
        }
        other => panic!("expected OutOfSync, got {other:?}"),
    }
    // The resumed stream delivers seq 20 after the transparent reconnect.
    match next_item(&mut stream).await {
        Some(Ok(ServerMessage::Event { seq, .. })) => assert_eq!(seq, 20),
        other => panic!("expected resumed Event 20, got {other:?}"),
    }

    stream.close().await;
    server.await.unwrap();
}

#[tokio::test]
async fn private_channel_without_credentials_fails_fast() {
    let client = Client::new(Config::with_base_url("http://unused"));
    let err = client.subscribe(vec![Channel::Orders]).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Terminal(nexus_exchange::TerminalError::Credentials(_))
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn backpressure_surfaces_lagged_and_preserves_order() {
    const BURST: u64 = 200;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        let _sub = read_text(&mut ws).await;
        for seq in 0..BURST {
            send_json(
                &mut ws,
                json!({ "op": "event", "channel": "trades", "market": "BTC-USDX-PERP", "seq": seq, "payload": {} }),
            )
            .await;
        }
        ws.close(None).await.unwrap();
    });

    // A small capacity exposes the consumer to backpressure under the burst.
    let client = client_for(addr, 4);
    let mut stream = client
        .subscribe(vec![Channel::trades("BTC-USDX-PERP")])
        .unwrap();

    // Delivery is order-preserving and gap-aware: each frame's seq equals the
    // running count of (delivered + reported-lagged), so a reorder/duplicate
    // trips the assert, and dropped frames are accounted for by a `Lagged`.
    let mut expected: u64 = 0;
    let mut delivered: u64 = 0;
    let mut lagged: u64 = 0;
    // Drain until a short lull: once the burst connection closes, the client
    // reconnects to the now-dead port and no further frames arrive, so a brief
    // timeout cleanly ends the loop (covering the case where trailing drops
    // after the last delivered frame are never flushed).
    while let Ok(item) = tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
        match item {
            Some(Ok(ServerMessage::Event { seq, .. })) => {
                assert_eq!(
                    seq, expected,
                    "reorder/duplicate: got {seq}, want {expected}"
                );
                expected += 1;
                delivered += 1;
            }
            Some(Err(Error::Transient(nexus_exchange::TransientError::Lagged { dropped }))) => {
                assert!(dropped > 0);
                expected += dropped;
                lagged += dropped;
            }
            Some(Ok(other)) => panic!("unexpected frame: {other:?}"),
            Some(Err(_)) | None => break,
        }
    }

    assert!(delivered >= 1, "expected at least one delivered frame");
    assert!(delivered + lagged <= BURST);

    stream.close().await;
    let _ = server.await;
}
