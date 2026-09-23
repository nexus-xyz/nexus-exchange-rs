//! Integration tests for the typed, protocol-aware streaming client
//! ([`Client::subscribe`] / [`MessageStream`]), driven against a local
//! `tokio-tungstenite` server. They exercise the two things this layer adds over
//! the raw client: op-envelope decoding and **cursor-based resume** — on
//! reconnect the client must replay each `subscribe` with a `since` cursor equal
//! to the last `seq` it processed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nexus_exchange::ws::{Channel, MessageStream, ServerMessage};
use nexus_exchange::{Client, Config, Error, TerminalError};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// A client wired to a local ws:// server with a fast, deterministic backoff so
/// reconnect tests don't actually wait seconds.
#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
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
#[allow(deprecated)] // Throwaway test origin; the selector stays supported.
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

/// A plain-HTTP server that answers every WebSocket upgrade with `status_line`
/// and `body` instead of `101`, counting the attempts it receives.
async fn refusing_server(
    status_line: &'static str,
    body: &'static str,
) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            // Read the upgrade request head, then refuse it.
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (addr, attempts)
}

/// Wait until `attempts` reaches `n`, failing the test after a hard bound.
async fn wait_for_attempts(attempts: &AtomicUsize, n: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while attempts.load(Ordering::SeqCst) < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {n} attempts, saw {}",
            attempts.load(Ordering::SeqCst)
        )
    });
}

/// Typed stream, public channel, no credentials: a `401` upgrade yields one
/// `TerminalError::Auth` and the stream ends after exactly one attempt.
#[tokio::test]
async fn typed_tokenless_401_is_permanent_and_typed() {
    let (addr, attempts) =
        refusing_server("401 Unauthorized", r#"{"code":"ws_token_missing"}"#).await;
    let mut stream = client_for(addr, 16)
        .subscribe(vec![Channel::trades("BTC-USDX-PERP")])
        .unwrap();
    let item = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("an item");
    match item {
        Some(Err(Error::Terminal(TerminalError::Auth { code, message }))) => {
            assert_eq!(code, "ws_token_missing");
            assert!(message.contains("requires a token"), "{message}");
        }
        other => panic!("expected TerminalError::Auth, got {other:?}"),
    }
    let end = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream ends");
    assert!(end.is_none(), "stream must end, got {end:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

/// Typed stream: a `503` upgrade is transient and retried transparently.
#[tokio::test]
async fn typed_upgrade_503_keeps_retrying() {
    let (addr, attempts) = refusing_server("503 Service Unavailable", "{}").await;
    let stream = client_for(addr, 16)
        .subscribe(vec![Channel::trades("BTC-USDX-PERP")])
        .unwrap();
    wait_for_attempts(&attempts, 3).await;
    drop(stream);
}
