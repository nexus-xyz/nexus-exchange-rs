//! WebSocket streaming client (`/ws`).
//!
//! A thin, idiomatic wrapper over the Nexus Exchange WebSocket surface. The
//! transport is `tokio-tungstenite`; this module adds typed [`Subscription`]
//! and [`ServerMessage`] envelopes that mirror the server's `op`-tagged JSON
//! wire protocol, plus a [`WsClient`] that implements
//! [`futures_util::Stream`].
//!
//! ## Connecting
//!
//! The handshake consumes a single-use bearer token (60 s TTL) minted via the
//! REST endpoint `POST /ws/token` (built elsewhere — this client just takes a
//! `token: &str`). The token rides on the URL query string:
//! `wss://<host>/ws?token=<TOKEN>`.
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! use futures_util::StreamExt;
//! use nexus_exchange::ws::{Subscription, WsClient};
//!
//! let mut ws = WsClient::connect("https://exchange.nexus.xyz", "my-token").await?;
//! ws.subscribe(Subscription::Trades {
//!     market: "BTC-USDX-PERP".into(),
//!     since: None,
//! })
//! .await?;
//!
//! while let Some(msg) = ws.next().await {
//!     println!("{:?}", msg?);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Reconnection
//!
//! This client does not manage auto-reconnect. The cursors needed to resume a
//! stream are surfaced on the types: [`ServerMessage::Subscribed`] carries
//! `seq_at_join` and [`ServerMessage::Event`] carries `seq`. Persist the last
//! `seq` you processed and pass it as `since` on the next [`Subscription`]
//! after reconnecting. If the server's ring buffer has overrun your cursor it
//! replies [`ServerMessage::OutOfSync`]; refetch via REST and resubscribe
//! without a `since`.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;

/// Errors surfaced by the WebSocket client.
///
/// Kept local to this module so the streaming surface can evolve without
/// touching the crate-wide [`crate::Error`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WsError {
    /// The base URL could not be turned into a `ws`/`wss` endpoint.
    #[error("invalid url: {0}")]
    Url(String),

    /// Transport-layer WebSocket failure (handshake, framing, I/O). Boxed —
    /// the underlying tungstenite error is large.
    #[error("websocket error: {0}")]
    Transport(#[from] Box<tokio_tungstenite::tungstenite::Error>),

    /// Failed to (de)serialize a wire envelope.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// A non-text frame arrived where a JSON envelope was expected.
    #[error("unexpected non-text frame")]
    UnexpectedFrame,

    /// The server closed the connection with a non-normal close code (e.g.
    /// token expiry, policy violation) rather than a graceful close. Surfaced
    /// so callers can distinguish it from a clean end-of-stream.
    #[error("connection closed by server (code {code}): {reason}")]
    Closed {
        /// WebSocket close code.
        code: u16,
        /// Server-supplied close reason, if any.
        reason: String,
    },
}

/// The engine sequence envelope (`epoch`, `sequence`, `emitted_at`) that rides
/// on real engine-originated events. Absent for events the indexer
/// synthesizes (e.g. snapshot-on-subscribe seeds). Use `sequence` to detect
/// cross-process gaps the per-channel `seq` cursor cannot reveal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineEnvelope {
    /// Engine epoch counter.
    pub epoch: u32,
    /// Monotonic engine sequence, durable across indexer restarts.
    pub sequence: u64,
    /// Unix-ms emission timestamp from the engine.
    pub emitted_at: u64,
}

/// A channel subscription request.
///
/// Public channels (`Trades`, `Book`, `Candles`) require a `market`.
/// Per-account channels (`Orders`, `Fills`, `Positions`, `Balances`) are
/// filtered server-side to the authenticated wallet and take no market.
///
/// `since` is an optional reconnect cursor — the server replays buffered
/// events with `seq > since`, or replies [`ServerMessage::OutOfSync`] if the
/// cursor predates its ring buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subscription {
    /// Public per-market trade stream.
    Trades {
        /// Market id, e.g. `BTC-USDX-PERP`.
        market: String,
        /// Optional reconnect cursor.
        since: Option<u64>,
    },
    /// Public per-market order-book stream.
    Book {
        /// Market id.
        market: String,
        /// Optional reconnect cursor.
        since: Option<u64>,
    },
    /// Public per-market candle stream.
    Candles {
        /// Market id.
        market: String,
        /// Optional reconnect cursor.
        since: Option<u64>,
    },
    /// Per-account order updates.
    Orders {
        /// Optional reconnect cursor.
        since: Option<u64>,
    },
    /// Per-account fill updates.
    Fills {
        /// Optional reconnect cursor.
        since: Option<u64>,
    },
    /// Per-account position updates.
    Positions {
        /// Optional reconnect cursor.
        since: Option<u64>,
    },
    /// Per-account balance updates.
    Balances {
        /// Optional reconnect cursor.
        since: Option<u64>,
    },
}

impl Subscription {
    /// The wire channel name.
    pub fn channel(&self) -> &'static str {
        match self {
            Subscription::Trades { .. } => "trades",
            Subscription::Book { .. } => "book",
            Subscription::Candles { .. } => "candles",
            Subscription::Orders { .. } => "orders",
            Subscription::Fills { .. } => "fills",
            Subscription::Positions { .. } => "positions",
            Subscription::Balances { .. } => "balances",
        }
    }

    /// The market this subscription targets, if any. `None` for per-account
    /// channels.
    pub fn market(&self) -> Option<&str> {
        match self {
            Subscription::Trades { market, .. }
            | Subscription::Book { market, .. }
            | Subscription::Candles { market, .. } => Some(market),
            _ => None,
        }
    }

    fn since(&self) -> Option<u64> {
        match self {
            Subscription::Trades { since, .. }
            | Subscription::Book { since, .. }
            | Subscription::Candles { since, .. }
            | Subscription::Orders { since }
            | Subscription::Fills { since }
            | Subscription::Positions { since }
            | Subscription::Balances { since } => *since,
        }
    }

    fn to_subscribe_op(&self) -> ClientOp {
        ClientOp::Subscribe {
            channel: self.channel(),
            market: self.market().map(str::to_string),
            since: self.since(),
        }
    }

    fn to_unsubscribe_op(&self) -> ClientOp {
        ClientOp::Unsubscribe {
            channel: self.channel(),
            market: self.market().map(str::to_string),
        }
    }
}

/// Client → server op envelope. Serializes to the `op`-tagged JSON the server
/// expects (`{"op":"subscribe", ...}`).
#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ClientOp {
    Subscribe {
        channel: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        market: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
    },
    Unsubscribe {
        channel: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        market: Option<String>,
    },
}

/// Server → client envelope, tagged on the `op` field.
///
/// Per-channel event payloads are left as raw [`serde_json::Value`] for now —
/// the SDK does not type them yet.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    /// Acknowledges a successful subscribe. `seq_at_join` is the server's
    /// current cursor at attach time — the starting `since` for a reconnect.
    Subscribed {
        /// Channel name.
        channel: String,
        /// Target market, for public channels.
        market: Option<String>,
        /// Server cursor at the moment the subscription attached.
        seq_at_join: u64,
    },
    /// Acknowledges an unsubscribe.
    Unsubscribed {
        /// Channel name.
        channel: String,
        /// Target market, for public channels.
        market: Option<String>,
    },
    /// A delivered channel event. `seq` is monotonic per channel; persist it
    /// to resume via `since` on reconnect.
    Event {
        /// Channel name.
        channel: String,
        /// Target market, for public channels.
        market: Option<String>,
        /// Per-channel monotonic sequence.
        seq: u64,
        /// Engine envelope, present for engine-originated events.
        #[serde(default)]
        engine_envelope: Option<EngineEnvelope>,
        /// Opaque event payload.
        payload: Value,
    },
    /// The server's ring buffer overran your `since` cursor. Refetch via REST
    /// and resubscribe. `oldest_seq` is the lowest cursor still replayable
    /// (`null` when nothing is buffered).
    OutOfSync {
        /// Channel name.
        channel: String,
        /// Target market, for public channels.
        market: Option<String>,
        /// Oldest replayable cursor, or `None` when the buffer is empty.
        oldest_seq: Option<u64>,
    },
    /// A server-side error (bad op, unknown channel, missing market, etc.).
    Error {
        /// Human-readable error message.
        message: String,
    },
}

/// A typed WebSocket stream over the Nexus Exchange `/ws` endpoint.
///
/// Implements [`Stream`] yielding `Result<ServerMessage, WsError>`. Use
/// [`WsClient::subscribe`] / [`WsClient::unsubscribe`] to manage channels.
#[derive(Debug)]
pub struct WsClient {
    inner: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

impl WsClient {
    /// Connect to the indexer's `/ws` endpoint, authenticating with a
    /// single-use `token` (mint one via `POST /ws/token`).
    ///
    /// `base_url` is the indexer's WebSocket origin (e.g. `wss://<indexer-host>`
    /// or `ws://localhost:9090`). WebSockets connect **directly to the indexer
    /// at host-root `/ws`** — the `/api/exchange` HTTP gateway cannot proxy WS
    /// upgrades — so any path on `base_url` (such as `/api/exchange`) is
    /// dropped, and the scheme is rewritten to `ws`/`wss`.
    ///
    /// The single-use `token` is carried as a `?token=` query parameter on the
    /// handshake URL (percent-encoded), so treat the connect URL as a secret —
    /// do not log it.
    pub async fn connect(base_url: &str, token: &str) -> Result<Self, WsError> {
        let url = ws_url(base_url, token)?;
        let (inner, _resp) = connect_async(url).await.map_err(Box::new)?;
        Ok(Self { inner })
    }

    /// Send a `subscribe` op for the given [`Subscription`].
    pub async fn subscribe(&mut self, sub: Subscription) -> Result<(), WsError> {
        self.send_op(&sub.to_subscribe_op()).await
    }

    /// Send an `unsubscribe` op for the given [`Subscription`]. Only the
    /// channel + market are sent; `since` is ignored.
    pub async fn unsubscribe(&mut self, sub: Subscription) -> Result<(), WsError> {
        self.send_op(&sub.to_unsubscribe_op()).await
    }

    /// Close the connection gracefully.
    pub async fn close(&mut self) -> Result<(), WsError> {
        self.inner.close(None).await.map_err(Box::new)?;
        Ok(())
    }

    async fn send_op(&mut self, op: &ClientOp) -> Result<(), WsError> {
        let text = serde_json::to_string(op)?;
        self.inner
            .send(Message::text(text))
            .await
            .map_err(Box::new)?;
        Ok(())
    }
}

impl Stream for WsClient {
    type Item = Result<ServerMessage, WsError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match self.inner.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(msg))) => match msg {
                    Message::Text(text) => {
                        return Poll::Ready(Some(
                            serde_json::from_str::<ServerMessage>(&text).map_err(WsError::from),
                        ));
                    }
                    // Control frames are handled by tungstenite; skip and poll
                    // again so callers only ever see data envelopes.
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(frame) => {
                        return match frame {
                            // A non-normal close (e.g. token expiry / policy)
                            // surfaces its code+reason; a normal or frameless
                            // close is a clean end-of-stream.
                            Some(cf) if cf.code != CloseCode::Normal => {
                                Poll::Ready(Some(Err(WsError::Closed {
                                    code: cf.code.into(),
                                    reason: cf.reason.to_string(),
                                })))
                            }
                            _ => Poll::Ready(None),
                        };
                    }
                    Message::Binary(_) | Message::Frame(_) => {
                        return Poll::Ready(Some(Err(WsError::UnexpectedFrame)));
                    }
                },
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(WsError::from(Box::new(e)))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Derive a `ws`/`wss` host-root `/ws?token=…` URL from a base URL.
///
/// `http` → `ws`, `https` → `wss`. The WebSocket lives at the **host root**
/// `/ws` — WS connects directly to the indexer, and the `/api/exchange` HTTP
/// gateway cannot proxy upgrades — so any path on `base_url` is dropped.
fn ws_url(base_url: &str, token: &str) -> Result<String, WsError> {
    let parsed = Url::parse(base_url).map_err(|e| WsError::Url(format!("{base_url}: {e}")))?;
    let scheme = match parsed.scheme() {
        "https" | "wss" => "wss",
        "http" | "ws" => "ws",
        other => {
            return Err(WsError::Url(format!(
                "unsupported scheme '{other}' (want http(s):// or ws(s)://): {base_url}"
            )));
        }
    };
    let host = parsed
        .host_str()
        .ok_or_else(|| WsError::Url(format!("base url has no host: {base_url}")))?;
    // WS lives at host-root `/ws`; keep only host[:port] and drop any path
    // (e.g. `/api/exchange`) — the HTTP gateway can't proxy WS upgrades.
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();

    // Build via `Url` so the token is percent-encoded — removes the previous
    // load-bearing assumption that the minted token is already URL-safe.
    let mut url = Url::parse(&format!("{scheme}://{host}{port}/ws"))
        .map_err(|e| WsError::Url(format!("{base_url}: {e}")))?;
    url.query_pairs_mut().append_pair("token", token);
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_rewrites_scheme_and_appends_path() {
        assert_eq!(
            ws_url("https://exchange.nexus.xyz", "tok").unwrap(),
            "wss://exchange.nexus.xyz/ws?token=tok"
        );
        assert_eq!(
            ws_url("http://localhost:9090", "tok").unwrap(),
            "ws://localhost:9090/ws?token=tok"
        );
    }

    #[test]
    fn ws_url_uses_host_root_dropping_any_path() {
        // WS connects directly to the indexer at host-root /ws; the
        // /api/exchange HTTP gateway can't proxy upgrades, so the path is dropped.
        assert_eq!(
            ws_url("https://exchange.nexus.xyz/api/exchange/", "abc").unwrap(),
            "wss://exchange.nexus.xyz/ws?token=abc"
        );
    }

    #[test]
    fn ws_url_accepts_ws_scheme_passthrough() {
        assert_eq!(
            ws_url("ws://host:1234", "t").unwrap(),
            "ws://host:1234/ws?token=t"
        );
    }

    #[test]
    fn ws_url_rejects_unknown_scheme() {
        assert!(matches!(ws_url("ftp://host", "t"), Err(WsError::Url(_))));
        assert!(matches!(ws_url("https://", "t"), Err(WsError::Url(_))));
    }

    #[test]
    fn ws_url_percent_encodes_token() {
        // URL-significant characters in the token must be encoded, not appended
        // raw — so connection no longer relies on the token being URL-safe.
        assert_eq!(
            ws_url("https://host", "a b/c+d").unwrap(),
            "wss://host/ws?token=a+b%2Fc%2Bd"
        );
    }

    #[test]
    fn subscribe_op_serializes_public_channel() {
        let op = Subscription::Trades {
            market: "BTC-USDX-PERP".into(),
            since: None,
        }
        .to_subscribe_op();
        let json: Value = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op":"subscribe","channel":"trades","market":"BTC-USDX-PERP"})
        );
    }

    #[test]
    fn subscribe_op_serializes_since_cursor() {
        let op = Subscription::Book {
            market: "ETH-USDX-PERP".into(),
            since: Some(42),
        }
        .to_subscribe_op();
        let json: Value = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op":"subscribe","channel":"book","market":"ETH-USDX-PERP","since":42})
        );
    }

    #[test]
    fn subscribe_op_omits_market_for_account_channel() {
        let op = Subscription::Fills { since: None }.to_subscribe_op();
        let json: Value = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op":"subscribe","channel":"fills"})
        );
    }

    #[test]
    fn unsubscribe_op_serializes() {
        let op = Subscription::Positions { since: Some(9) }.to_unsubscribe_op();
        let json: Value = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
        // since is dropped on unsubscribe; positions carries no market.
        assert_eq!(
            json,
            serde_json::json!({"op":"unsubscribe","channel":"positions"})
        );
    }

    #[test]
    fn deserialize_subscribed() {
        let msg: ServerMessage = serde_json::from_str(
            r#"{"op":"subscribed","channel":"trades","market":"BTC-USDX-PERP","seq_at_join":7}"#,
        )
        .unwrap();
        assert_eq!(
            msg,
            ServerMessage::Subscribed {
                channel: "trades".into(),
                market: Some("BTC-USDX-PERP".into()),
                seq_at_join: 7,
            }
        );
    }

    #[test]
    fn deserialize_event_with_engine_envelope() {
        let msg: ServerMessage = serde_json::from_str(
            r#"{"op":"event","channel":"fills","market":null,"seq":3,
                "engine_envelope":{"epoch":1,"sequence":1000,"emitted_at":123},
                "payload":{"qty":"1.5"}}"#,
        )
        .unwrap();
        match msg {
            ServerMessage::Event {
                channel,
                market,
                seq,
                engine_envelope,
                payload,
            } => {
                assert_eq!(channel, "fills");
                assert_eq!(market, None);
                assert_eq!(seq, 3);
                assert_eq!(
                    engine_envelope,
                    Some(EngineEnvelope {
                        epoch: 1,
                        sequence: 1000,
                        emitted_at: 123,
                    })
                );
                assert_eq!(payload, serde_json::json!({"qty":"1.5"}));
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_event_without_engine_envelope() {
        let msg: ServerMessage = serde_json::from_str(
            r#"{"op":"event","channel":"trades","market":"BTC-USDX-PERP","seq":1,"payload":{}}"#,
        )
        .unwrap();
        match msg {
            ServerMessage::Event {
                engine_envelope, ..
            } => assert_eq!(engine_envelope, None),
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_out_of_sync_and_error() {
        let oos: ServerMessage = serde_json::from_str(
            r#"{"op":"out_of_sync","channel":"orders","market":null,"oldest_seq":50}"#,
        )
        .unwrap();
        assert_eq!(
            oos,
            ServerMessage::OutOfSync {
                channel: "orders".into(),
                market: None,
                oldest_seq: Some(50),
            }
        );

        let err: ServerMessage =
            serde_json::from_str(r#"{"op":"error","message":"unknown_channel: bogus"}"#).unwrap();
        assert_eq!(
            err,
            ServerMessage::Error {
                message: "unknown_channel: bogus".into(),
            }
        );
    }

    #[test]
    fn channel_and_market_accessors() {
        let s = Subscription::Candles {
            market: "SOL-USDX-PERP".into(),
            since: None,
        };
        assert_eq!(s.channel(), "candles");
        assert_eq!(s.market(), Some("SOL-USDX-PERP"));

        let a = Subscription::Balances { since: None };
        assert_eq!(a.channel(), "balances");
        assert_eq!(a.market(), None);
    }
}
