//! The public market-data client (`/stream`) against a local mock server that
//! behaves like the indexer's `handle_ws_stream`: read exactly one client
//! message, never read again, push untagged-by-name frames.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nexus_exchange::stream::{MarketStream, StreamChannel, StreamEvent};
use nexus_exchange::ws::Backoff;
use nexus_exchange::{Client, Config, CustomNetwork, Funds, Network};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const MARKET: &str = "BTC-USDX-PERP";

fn fast_client() -> Client {
    Client::new(
        Config::new(Network::Local).with_reconnect_backoff(
            Backoff::new()
                .with_initial(Duration::from_millis(5))
                .with_max(Duration::from_millis(20)),
        ),
    )
}

async fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/stream", listener.local_addr().unwrap());
    (listener, url)
}

/// Accept one upgrade, returning the socket and the request path+query.
// The handshake callback's error type is tungstenite's, not ours to shrink.
#[allow(clippy::result_large_err)]
async fn accept(listener: &TcpListener) -> (WebSocketStream<TcpStream>, String) {
    let (sock, _) = listener.accept().await.unwrap();
    let mut uri = String::new();
    let ws = tokio_tungstenite::accept_hdr_async(sock, |req: &Request, resp: Response| {
        uri = req.uri().to_string();
        Ok(resp)
    })
    .await
    .unwrap();
    (ws, uri)
}

async fn first_message(ws: &mut WebSocketStream<TcpStream>) -> Value {
    match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected a text subscribe message, got {other:?}"),
    }
}

async fn send(ws: &mut WebSocketStream<TcpStream>, frame: Value) {
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

async fn next(stream: &mut MarketStream) -> StreamEvent {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("timed out waiting for an event")
        .expect("stream ended")
}

fn book_frame(sequence: u64) -> Value {
    json!({
        "type": "BookUpdate",
        "market_id": MARKET,
        "book": {
            "bids": [
                { "price": "64000.5", "quantity": "0.25", "order_count": 2 },
                { "price": "63999", "quantity": "1.5", "order_count": 1 }
            ],
            "asks": [{ "price": "64001.10", "quantity": "0.3", "order_count": 3 }],
            "sequence": sequence
        }
    })
}

/// The one first message is the untagged `{"subscribe": [...]}` with every
/// requested channel, on `/stream`, with no token.
#[tokio::test]
async fn sends_one_untagged_subscribe_with_every_channel() {
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let (mut ws, uri) = accept(&listener).await;
        (uri, first_message(&mut ws).await)
    });

    let mut stream = fast_client()
        .market_stream_at(url, StreamChannel::all_for(MARKET))
        .unwrap();
    assert!(matches!(next(&mut stream).await, StreamEvent::Connected));

    let (uri, msg) = server.await.unwrap();
    assert_eq!(uri, "/stream", "no token, no query");
    assert_eq!(
        msg,
        json!({ "subscribe": [
            "book:BTC-USDX-PERP",
            "trades:BTC-USDX-PERP",
            "market_status:BTC-USDX-PERP"
        ]}),
        "exactly one key, no `op`/`type` envelope"
    );
    stream.close().await;
}

/// `BookUpdate` levels are objects with decimal strings; `sequence` is exposed
/// as sent, jumps included.
#[tokio::test]
async fn parses_book_updates_with_object_levels() {
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let (mut ws, _) = accept(&listener).await;
        first_message(&mut ws).await;
        send(&mut ws, book_frame(15_995)).await;
        send(&mut ws, book_frame(16_210)).await; // not contiguous, and fine
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut stream = fast_client()
        .market_stream_at(url, vec![StreamChannel::book(MARKET)])
        .unwrap();
    assert!(matches!(next(&mut stream).await, StreamEvent::Connected));

    let StreamEvent::Book(first) = next(&mut stream).await else {
        panic!("expected a book");
    };
    assert_eq!(first.market_id, MARKET);
    assert_eq!(first.sequence, 15_995);
    assert_eq!(first.bids.len(), 2);
    assert_eq!(first.bids[0].price.to_string(), "64000.5");
    assert_eq!(first.bids[0].quantity.to_string(), "0.25");
    assert_eq!(first.bids[0].order_count, 2);
    assert_eq!(first.asks[0].price.to_string(), "64001.10");

    let StreamEvent::Book(second) = next(&mut stream).await else {
        panic!("expected a book");
    };
    assert_eq!(second.sequence, 16_210);
    assert!(second.received_at >= first.received_at);

    stream.close().await;
    server.abort();
}

/// `{"type":"gap","missed":n}` surfaces as a typed event, in order.
#[tokio::test]
async fn surfaces_server_gap() {
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let (mut ws, _) = accept(&listener).await;
        first_message(&mut ws).await;
        send(&mut ws, book_frame(1)).await;
        send(&mut ws, json!({ "type": "gap", "missed": 37 })).await;
        send(&mut ws, book_frame(9)).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut stream = fast_client()
        .market_stream_at(url, vec![StreamChannel::book(MARKET)])
        .unwrap();
    assert!(matches!(next(&mut stream).await, StreamEvent::Connected));
    assert!(matches!(next(&mut stream).await, StreamEvent::Book(b) if b.sequence == 1));
    assert!(matches!(
        next(&mut stream).await,
        StreamEvent::Gap { missed: 37 }
    ));
    assert!(matches!(next(&mut stream).await, StreamEvent::Book(b) if b.sequence == 9));
    stream.close().await;
    server.abort();
}

/// The load balancer's cut: the TCP connection vanishes with no close frame
/// (1006). The client reports it, reconnects, and sends the same subscribe
/// message as the first message of the new connection.
#[tokio::test]
async fn reconnects_after_1006_and_resubscribes() {
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let (mut ws, _) = accept(&listener).await;
        first_message(&mut ws).await;
        send(&mut ws, book_frame(1)).await;
        // Drop the TCP stream without a close handshake.
        drop(ws);

        let (mut ws, _) = accept(&listener).await;
        first_message(&mut ws).await;
        send(&mut ws, book_frame(2)).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let channels = vec![StreamChannel::book(MARKET), StreamChannel::trades(MARKET)];
    let mut stream = fast_client().market_stream_at(url, channels).unwrap();
    assert!(matches!(next(&mut stream).await, StreamEvent::Connected));
    assert!(matches!(next(&mut stream).await, StreamEvent::Book(b) if b.sequence == 1));
    match next(&mut stream).await {
        StreamEvent::Disconnected { reason } => {
            assert!(reason.contains("1006"), "{reason}")
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
    assert!(matches!(next(&mut stream).await, StreamEvent::Connected));
    assert!(matches!(next(&mut stream).await, StreamEvent::Book(b) if b.sequence == 2));

    stream.close().await;
    server.abort();
}

/// Same, but inspect the two first messages: identical, and complete.
#[tokio::test]
async fn resubscribe_repeats_the_first_message_verbatim() {
    let (listener, url) = bind().await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut ws, _) = accept(&listener).await;
            tx.send(first_message(&mut ws).await).unwrap();
            drop(ws);
        }
    });

    let stream = fast_client()
        .market_stream_at(url, StreamChannel::all_for(MARKET))
        .unwrap();
    let a = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let b = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(a["subscribe"].as_array().unwrap().len(), 3);
    stream.close().await;
    let _ = server.await;
}

/// The server reads only the first message. The client must never send a
/// second one on the same connection (nothing would read it), and must still
/// deliver everything the server pushes.
#[tokio::test]
async fn works_against_a_first_message_only_server() {
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let (mut ws, _) = accept(&listener).await;
        // Like the indexer: read the first message, then only write.
        first_message(&mut ws).await;
        for frame in [
            book_frame(3),
            json!({ "type": "MarketHalted", "market_id": MARKET, "reason": "oracle stale", "timestamp": 10 }),
            json!({ "type": "MarketResumed", "market_id": MARKET, "timestamp": 11 }),
            json!({ "type": "Stats", "stats": {} }),
        ] {
            send(&mut ws, frame).await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut stream = fast_client()
        .market_stream_at(url, StreamChannel::all_for(MARKET))
        .unwrap();
    assert!(matches!(next(&mut stream).await, StreamEvent::Connected));
    assert!(matches!(next(&mut stream).await, StreamEvent::Book(b) if b.sequence == 3));
    assert!(
        matches!(next(&mut stream).await, StreamEvent::MarketHalted { reason, timestamp: 10, .. } if reason == "oracle stale")
    );
    assert!(matches!(
        next(&mut stream).await,
        StreamEvent::MarketResumed { timestamp: 11, .. }
    ));
    // A frame type this version does not know is passed through, not dropped.
    assert!(matches!(
        next(&mut stream).await,
        StreamEvent::Unrecognized { .. }
    ));

    stream.close().await;
    server.abort();
}

/// The server-side view of the same contract: after the subscribe, the client
/// sends nothing a first-message-only server would silently ignore.
#[tokio::test]
async fn sends_nothing_after_the_first_message() {
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let (mut ws, _) = accept(&listener).await;
        first_message(&mut ws).await;
        send(&mut ws, book_frame(1)).await;
        send(&mut ws, book_frame(2)).await;
        tokio::time::timeout(Duration::from_millis(500), ws.next())
            .await
            .ok()
            .flatten()
    });

    let mut stream = fast_client()
        .market_stream_at(url, vec![StreamChannel::book(MARKET)])
        .unwrap();
    assert!(matches!(next(&mut stream).await, StreamEvent::Connected));
    assert!(matches!(next(&mut stream).await, StreamEvent::Book(_)));
    assert!(matches!(next(&mut stream).await, StreamEvent::Book(_)));
    let extra = server.await.unwrap();
    assert!(extra.is_none(), "client sent a second message: {extra:?}");
    stream.close().await;
}

#[tokio::test]
async fn refuses_locally_what_cannot_work() {
    // Mainnet's host does not resolve yet: refused before any I/O.
    let err = Client::new(Config::new(Network::Mainnet))
        .market_stream(StreamChannel::all_for(MARKET))
        .unwrap_err();
    assert!(err.to_string().contains("not targetable"), "{err}");

    // A custom network declares no /stream URL.
    let custom = Network::Custom(
        CustomNetwork::new("preview", "https://preview.example.invalid", Funds::Play).unwrap(),
    );
    let err = Client::new(Config::new(custom))
        .market_stream(StreamChannel::all_for(MARKET))
        .unwrap_err();
    assert!(err.to_string().contains("market_stream_at"), "{err}");

    let client = fast_client();
    assert!(client.market_stream(vec![]).is_err());
    assert!(client
        .market_stream_at("https://h/stream", vec![StreamChannel::book(MARKET)])
        .is_err());
}
