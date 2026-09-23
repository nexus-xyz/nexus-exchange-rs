//! Public market-data stream (`GET /stream`): no token, no credentials.
//!
//! This is a different socket from the account stream in [`crate::ws`], and it
//! speaks a different protocol. `/ws` needs a token from `POST /ws/token` even
//! for public channels and uses the `op` envelope (`seq`, `since`, acks).
//! `/stream` takes no token, and the protocol is much smaller:
//!
//! * **One subscribe message, and only the first one counts.** The client sends
//!   a single `{"subscribe": ["book:<market>", "trades:<market>", …]}` right
//!   after the upgrade. The server reads that one message and never reads the
//!   socket again, so there is no acknowledgement, no unsubscribe, and no way
//!   to change channels on a live connection. [`MarketStream`] therefore takes
//!   its whole channel set up front and re-sends it as the first message of
//!   every reconnect.
//! * **Unknown channels are silently empty.** The server accepts any string and
//!   forwards whatever matches it. A typo'd market subscribes to nothing, with
//!   no error, which is why [`StreamChannel`] builds the names.
//! * **Book frames are full top-20 snapshots, not deltas.** Replace the local
//!   book on every [`StreamEvent::Book`]; never merge. Levels carry decimal
//!   strings, parsed here into exact [`Decimal`]s.
//! * **`sequence` is monotonic, not contiguous.** [`BookSnapshot::sequence`]
//!   only ever increases on a connection and equals the `nonce` that
//!   `GET /markets/{market_id}/orderbook` serves, but it skips values, so a
//!   jump is **not** a missed update. Use it to order snapshots and to line one
//!   up with a REST read, not to detect loss.
//! * **Loss is reported, not replayed.** When this connection falls behind the
//!   server's broadcast buffer, the server sends `{"type":"gap","missed":n}`,
//!   surfaced as [`StreamEvent::Gap`]. `n` counts updates dropped across *all*
//!   channels, not only the subscribed ones. Nothing is resent: re-read
//!   anything that is not a snapshot (trades) over REST, and treat the
//!   book as stale until the next [`StreamEvent::Book`] or a REST refetch.
//! * **Frames carry no timestamp.** [`BookSnapshot::received_at`] is the local
//!   arrival time.
//! * **No ADL.** Auto-deleveraging settlements name the accounts involved, so
//!   the server withholds them from this unauthenticated socket (ENG-17189)
//!   and an `adl:` channel matches nothing. This client does not offer one.
//!   ADL history is on the authenticated REST reads. An `AdlSettlement` frame
//!   from a server that predates the change arrives as
//!   [`StreamEvent::Unrecognized`].
//!
//! # Connection lifetime
//!
//! The server sends no pings and has no idle timeout of its own, but on the
//! public hosts the load balancer closes every socket about 30 s after the
//! upgrade, with no close frame (the client sees an abnormal closure, 1006).
//! That is routine here, not a fault: the client reports
//! [`StreamEvent::Disconnected`], reconnects after a short jittered backoff and
//! sends the same subscribe message again. A connection that stayed up for a
//! few seconds, or delivered any frame, counts as healthy and resets the
//! backoff, so the 30 s cycle does not escalate the delay even when the books
//! are quiet.
//!
//! Anything published between the drop and the resubscribe is missed. Books
//! recover on their own (the next frame is a full snapshot); trades do not, so a consumer that needs every print should reconcile
//! against REST after each [`StreamEvent::Connected`] that follows a
//! [`StreamEvent::Disconnected`].
//!
//! # Example
//!
//! ```no_run
//! use nexus_exchange::stream::{StreamChannel, StreamEvent};
//! use nexus_exchange::{Client, Config, Network};
//!
//! # async fn run() -> nexus_exchange::Result<()> {
//! let client = Client::new(Config::new(Network::Testnet)); // no credentials
//! let mut stream = client.market_stream(StreamChannel::all_for("BTC-USDX-PERP"))?;
//! while let Some(event) = stream.next().await {
//!     match event {
//!         StreamEvent::Book(book) => println!("{} seq {}", book.market_id, book.sequence),
//!         StreamEvent::Gap { missed } => println!("server dropped {missed}; refetch over REST"),
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures_core::Stream;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::types::Trade;
use crate::ws::{Backoff, BackoffIter};
use crate::{Client, Error, Network, Result};

/// How long a connection must stay up to count as healthy (and reset the
/// backoff) even if it delivered nothing. Well under the ~30 s load-balancer
/// lifetime, well over a connect-then-refuse flap.
const HEALTHY_AFTER: Duration = Duration::from_secs(5);

// ── Channels ────────────────────────────────────────────────────────────────

/// A `/stream` channel. The market is a market id (`BTC-USDX-PERP`) or `*` for
/// every market.
///
/// `stats` and `adl` are deliberately absent: the server accepts `stats` but
/// nothing publishes on it, and withholds ADL settlements from this socket
/// (ENG-17189).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StreamChannel {
    /// `book:<market>`: full top-20 book snapshots, as [`StreamEvent::Book`].
    Book(String),
    /// `trades:<market>`: public trade prints, as [`StreamEvent::Trade`].
    Trades(String),
    /// `market_status:<market>`: [`StreamEvent::MarketHalted`] and
    /// [`StreamEvent::MarketResumed`].
    MarketStatus(String),
}

impl StreamChannel {
    /// `book:<market>`.
    pub fn book(market: impl Into<String>) -> Self {
        Self::Book(market.into())
    }

    /// `trades:<market>`.
    pub fn trades(market: impl Into<String>) -> Self {
        Self::Trades(market.into())
    }

    /// `market_status:<market>`.
    pub fn market_status(market: impl Into<String>) -> Self {
        Self::MarketStatus(market.into())
    }

    /// Every channel this client understands for one market: book, trades and
    /// market status.
    pub fn all_for(market: impl Into<String>) -> Vec<Self> {
        let market = market.into();
        vec![
            Self::Book(market.clone()),
            Self::Trades(market.clone()),
            Self::MarketStatus(market),
        ]
    }

    /// The market this channel filters on (`*` for all).
    pub fn market(&self) -> &str {
        match self {
            Self::Book(m) | Self::Trades(m) | Self::MarketStatus(m) => m,
        }
    }

    /// The channel name as the server matches it, e.g. `book:BTC-USDX-PERP`.
    pub fn wire_name(&self) -> String {
        let prefix = match self {
            Self::Book(_) => "book",
            Self::Trades(_) => "trades",
            Self::MarketStatus(_) => "market_status",
        };
        format!("{prefix}:{}", self.market())
    }
}

/// Build the single first message for `channels`, deduplicated in order.
///
/// Refuses an empty set, and an empty or whitespace-bearing market: the server
/// would accept either and deliver nothing, forever, with no error.
fn subscribe_message(channels: &[StreamChannel]) -> Result<String> {
    if channels.is_empty() {
        return Err(Error::invalid_request(
            "a /stream subscription needs at least one channel; the server reads only the \
             first message, so an empty one would deliver nothing for the whole connection",
        ));
    }
    let mut names: Vec<String> = Vec::with_capacity(channels.len());
    for channel in channels {
        let market = channel.market();
        if market.is_empty() || market.chars().any(char::is_whitespace) {
            return Err(Error::invalid_request(format!(
                "invalid /stream market {market:?}: the server would silently match nothing"
            )));
        }
        let name = channel.wire_name();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(serde_json::json!({ "subscribe": names }).to_string())
}

// ── Events ──────────────────────────────────────────────────────────────────

/// One price level of a [`BookSnapshot`].
///
/// An object on this socket (`{price, quantity, order_count}`), unlike the
/// `[price, amount]` pairs of `GET /markets/{market_id}/orderbook`. Price and
/// quantity arrive as decimal strings and are exact.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BookLevel {
    /// Level price.
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    /// Resting quantity at this price, in the base asset.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Number of resting orders at this price.
    pub order_count: u32,
}

/// A full top-20 order-book snapshot for one market. Replace, never merge.
#[derive(Debug, Clone, PartialEq)]
pub struct BookSnapshot {
    /// Market id, e.g. `BTC-USDX-PERP`.
    pub market_id: String,
    /// Bids, best first.
    pub bids: Vec<BookLevel>,
    /// Asks, best first.
    pub asks: Vec<BookLevel>,
    /// The book's projection sequence. Monotonic but **not contiguous**: it
    /// skips values, so a jump is not a missed update. Equal to the REST
    /// order book's `nonce` for the same state. See the [module
    /// docs](crate::stream).
    pub sequence: u64,
    /// When this client received the frame. The server sends no timestamp.
    pub received_at: SystemTime,
}

/// An event from a [`MarketStream`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StreamEvent {
    /// A socket was (re)opened and the subscribe message sent. The server
    /// sends no acknowledgement, so this is as confirmed as it gets.
    Connected,
    /// The socket dropped and the client is backing off before reconnecting.
    /// On the public hosts this happens about every 30 s by design; see the
    /// [module docs](crate::stream#connection-lifetime).
    Disconnected {
        /// Human-readable cause.
        reason: String,
    },
    /// A full book snapshot (`BookUpdate`).
    Book(BookSnapshot),
    /// A public trade print. `price`, `amount` and `cost` arrive as JSON
    /// numbers on this socket; see [`Trade`].
    Trade {
        /// Market id.
        market_id: String,
        /// The print.
        trade: Trade,
    },
    /// The market was halted.
    MarketHalted {
        /// Market id.
        market_id: String,
        /// Why, as the server states it.
        reason: String,
        /// Unix milliseconds.
        timestamp: u64,
    },
    /// The market resumed trading.
    MarketResumed {
        /// Market id.
        market_id: String,
        /// Unix milliseconds.
        timestamp: u64,
    },
    /// The **server** dropped `missed` updates because this connection fell
    /// behind its broadcast buffer. Counts every channel, not just the
    /// subscribed ones. Nothing is replayed: refetch over REST.
    Gap {
        /// Updates the server dropped.
        missed: u64,
    },
    /// The **consumer** fell behind this client's bounded channel and `dropped`
    /// events were discarded to keep the socket drained. Treat like
    /// [`Gap`](Self::Gap).
    Lagged {
        /// Events this client dropped.
        dropped: u64,
    },
    /// A JSON frame this version could not decode: a new `type`, or a known one
    /// whose shape drifted. Passed through rather than dropped.
    Unrecognized {
        /// The raw frame.
        frame: Value,
        /// Why decoding failed.
        error: String,
    },
}

/// The wire frames, as the indexer's untagged-by-name `IndexerUpdate` plus the
/// `gap` notice serialize them.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireFrame {
    BookUpdate {
        market_id: String,
        book: WireBook,
    },
    Trade {
        market_id: String,
        trade: Trade,
    },
    MarketHalted {
        market_id: String,
        reason: String,
        timestamp: u64,
    },
    MarketResumed {
        market_id: String,
        timestamp: u64,
    },
    #[serde(rename = "gap")]
    Gap {
        missed: u64,
    },
}

#[derive(Deserialize)]
struct WireBook {
    bids: Vec<BookLevel>,
    asks: Vec<BookLevel>,
    #[serde(default)]
    sequence: u64,
}

/// Decode one text frame. `None` for a frame that is not JSON at all.
fn decode(text: &str, received_at: SystemTime) -> Option<StreamEvent> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some(match WireFrame::deserialize(&value) {
        Ok(WireFrame::BookUpdate { market_id, book }) => StreamEvent::Book(BookSnapshot {
            market_id,
            bids: book.bids,
            asks: book.asks,
            sequence: book.sequence,
            received_at,
        }),
        Ok(WireFrame::Trade { market_id, trade }) => StreamEvent::Trade { market_id, trade },
        Ok(WireFrame::MarketHalted {
            market_id,
            reason,
            timestamp,
        }) => StreamEvent::MarketHalted {
            market_id,
            reason,
            timestamp,
        },
        Ok(WireFrame::MarketResumed {
            market_id,
            timestamp,
        }) => StreamEvent::MarketResumed {
            market_id,
            timestamp,
        },
        Ok(WireFrame::Gap { missed }) => StreamEvent::Gap { missed },
        Err(err) => StreamEvent::Unrecognized {
            frame: value,
            error: err.to_string(),
        },
    })
}

// ── URL ─────────────────────────────────────────────────────────────────────

impl Network {
    /// The public market-data socket (`/stream`) for this network, or `None`
    /// for a [`Custom`](Network::Custom) network (pass its URL to
    /// [`Client::market_stream_at`]).
    ///
    /// It is the spec's `rest_base` with the scheme swapped, plus `/stream`:
    /// `wss://api.testnet.nexus.xyz/v1/stream`. The `/v1` prefix is required;
    /// the public hosts route no WebSocket path at their root (bare `/stream`
    /// answers `404`). [`Mainnet`](Network::Mainnet) reports the shape its host
    /// will serve, but `api.nexus.xyz` does not resolve yet, and
    /// [`Client::market_stream`] refuses it locally.
    pub fn stream_url(&self) -> Option<&'static str> {
        match self {
            Network::Testnet => Some("wss://api.testnet.nexus.xyz/v1/stream"),
            Network::Mainnet => Some("wss://api.nexus.xyz/v1/stream"),
            Network::Local => Some("ws://localhost:9090/stream"),
            Network::Custom(_) => None,
        }
    }
}

// ── Client entry points ─────────────────────────────────────────────────────

impl Client {
    /// Open the public market-data stream for this network and subscribe to
    /// `channels`. Needs no credentials.
    ///
    /// Returns immediately; the socket opens in a background task that emits
    /// [`StreamEvent::Connected`], reconnects with the configured
    /// [`Backoff`] and re-sends the same subscribe message on every reconnect.
    /// The channel set is fixed for the stream's life, because the server
    /// reads only the first message; open another stream to change it.
    ///
    /// # Errors
    ///
    /// Locally, before any network I/O: [`Network::Mainnet`] (its host does
    /// not resolve yet), a [`Network::Custom`] network (use
    /// [`market_stream_at`](Self::market_stream_at)), an empty channel set, or
    /// an empty market name.
    ///
    /// Must be called from within a Tokio runtime (it spawns a task).
    pub fn market_stream(&self, channels: Vec<StreamChannel>) -> Result<MarketStream> {
        if matches!(self.config.network, Network::Mainnet) {
            return Err(Error::invalid_request(
                crate::client::MAINNET_NOT_TARGETABLE,
            ));
        }
        let url = self.config.network.stream_url().ok_or_else(|| {
            Error::invalid_request(
                "this network declares no public /stream URL; pass one with \
                 Client::market_stream_at",
            )
        })?;
        self.market_stream_at(url, channels)
    }

    /// [`market_stream`](Self::market_stream) against an explicit `ws://` or
    /// `wss://` URL, e.g. a preview deployment's `/stream`.
    ///
    /// # Errors
    ///
    /// An empty channel set or market name, or a URL that is not `ws(s)://`.
    ///
    /// Must be called from within a Tokio runtime (it spawns a task).
    pub fn market_stream_at(
        &self,
        url: impl Into<String>,
        channels: Vec<StreamChannel>,
    ) -> Result<MarketStream> {
        let url = url.into();
        if !(url.starts_with("ws://") || url.starts_with("wss://")) {
            return Err(Error::invalid_request(format!(
                "a /stream URL must be ws:// or wss://, got {url:?}"
            )));
        }
        let first_message = subscribe_message(&channels)?;

        let (event_tx, event_rx) = mpsc::channel(self.config.ws.channel_capacity);
        let (close_tx, close_rx) = mpsc::channel(1);
        let handle = tokio::spawn(run(
            RunConfig {
                url,
                first_message,
                backoff: self.config.ws.backoff.clone(),
                user_agent: self.config.user_agent.clone(),
            },
            event_tx,
            close_rx,
        ));
        Ok(MarketStream {
            rx: event_rx,
            close_tx,
            handle: Some(handle),
        })
    }
}

// ── The handle ──────────────────────────────────────────────────────────────

/// A live public market-data stream. Created by [`Client::market_stream`].
///
/// Pull events with [`next`](Self::next), or use it as a
/// [`Stream`]. Dropping it (or calling
/// [`close`](Self::close)) stops the background task.
#[derive(Debug)]
pub struct MarketStream {
    rx: mpsc::Receiver<StreamEvent>,
    close_tx: mpsc::Sender<()>,
    /// `Some` until [`close`](Self::close) takes it; `Drop` aborts whatever
    /// remains so the task never outlives the handle.
    handle: Option<JoinHandle<()>>,
}

impl MarketStream {
    /// The next event, or `None` once the stream has shut down.
    pub async fn next(&mut self) -> Option<StreamEvent> {
        self.rx.recv().await
    }

    /// Close the socket and wait for the background task to end.
    pub async fn close(mut self) {
        let _ = self.close_tx.send(()).await;
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Stream for MarketStream {
    type Item = StreamEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for MarketStream {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

// ── Background task ─────────────────────────────────────────────────────────

struct RunConfig {
    url: String,
    /// The one subscribe message, sent first on every connection.
    first_message: String,
    backoff: Backoff,
    user_agent: String,
}

enum Exit {
    Reconnect(String),
    Stop,
}

/// Emit without blocking; `false` once the consumer is gone. A full channel
/// drops a lifecycle marker rather than stall the read loop.
fn emit(tx: &mpsc::Sender<StreamEvent>, event: StreamEvent) -> bool {
    !matches!(tx.try_send(event), Err(TrySendError::Closed(_)))
}

async fn run(config: RunConfig, tx: mpsc::Sender<StreamEvent>, mut close_rx: mpsc::Receiver<()>) {
    let mut delays = config.backoff.iter();
    loop {
        let connect = match crate::ws::handshake_request(&config.url, &config.user_agent) {
            Ok(request) => tokio_tungstenite::connect_async(request).await,
            Err(err) => Err(err),
        };
        match connect {
            Ok((socket, _)) => {
                if !emit(&tx, StreamEvent::Connected) {
                    return;
                }
                let opened = tokio::time::Instant::now();
                let mut delivered = false;
                let exit = serve(
                    socket,
                    &config.first_message,
                    &tx,
                    &mut close_rx,
                    &mut delivered,
                )
                .await;
                // Healthy if it carried data or simply lived a while. The
                // second arm matters on quiet books: the load balancer's 30 s
                // close must not ratchet the backoff up to its cap.
                if delivered || opened.elapsed() >= HEALTHY_AFTER {
                    delays.reset();
                }
                match exit {
                    Exit::Stop => return,
                    Exit::Reconnect(reason) => {
                        if !emit(&tx, StreamEvent::Disconnected { reason }) {
                            return;
                        }
                    }
                }
            }
            Err(err) => {
                let reason = format!("connect failed: {err}");
                if !emit(&tx, StreamEvent::Disconnected { reason }) {
                    return;
                }
            }
        }
        if wait(&mut delays, &mut close_rx).await {
            return;
        }
    }
}

/// Sleep one backoff delay; `true` if a close (or a dropped handle) cut it short.
async fn wait(delays: &mut BackoffIter, close_rx: &mut mpsc::Receiver<()>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delays.next_delay()) => false,
        _ = close_rx.recv() => true,
    }
}

async fn serve<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    first_message: &str,
    tx: &mpsc::Sender<StreamEvent>,
    close_rx: &mut mpsc::Receiver<()>,
    delivered: &mut bool,
) -> Exit
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut write, mut read) = socket.split();

    // The only message the server will ever read on this connection.
    if let Err(err) = write.send(Message::Text(first_message.into())).await {
        return Exit::Reconnect(format!("subscribe send failed: {err}"));
    }

    let mut dropped: u64 = 0;
    loop {
        tokio::select! {
            frame = read.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(bytes))) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    Some(Ok(Message::Ping(payload))) => {
                        // The server sends none today; answer one if it starts.
                        if let Err(err) = write.send(Message::Pong(payload)).await {
                            return Exit::Reconnect(format!("pong send failed: {err}"));
                        }
                        continue;
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return Exit::Reconnect(match frame {
                            Some(f) => format!("server closed: {} {}", f.code, f.reason),
                            None => "server closed".to_string(),
                        });
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(err)) => return Exit::Reconnect(read_error(&err)),
                    None => return Exit::Reconnect("stream ended".to_string()),
                };
                if let Some(event) = decode(&text, SystemTime::now()) {
                    if !deliver(tx, event, delivered, &mut dropped) {
                        return Exit::Stop;
                    }
                }
            }
            _ = close_rx.recv() => {
                // Best effort: the server never reads again, so it will not
                // acknowledge this. Don't wait for a reply.
                let _ = write.send(Message::Close(None)).await;
                return Exit::Stop;
            }
        }
    }
}

/// Describe a read failure, naming the routine load-balancer cut.
fn read_error(err: &WsError) -> String {
    match err {
        WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
            "connection dropped without a close frame (1006); the public load balancer \
             ends every socket at ~30 s, reconnecting"
                .to_string()
        }
        other => format!("read error: {other}"),
    }
}

/// Hand an event to the consumer without blocking. On a full channel it is
/// dropped and counted, and the count goes out as [`StreamEvent::Lagged`]
/// ahead of the next delivered event. `false` once the consumer is gone.
fn deliver(
    tx: &mpsc::Sender<StreamEvent>,
    event: StreamEvent,
    delivered: &mut bool,
    dropped: &mut u64,
) -> bool {
    if *dropped > 0 {
        match tx.try_send(StreamEvent::Lagged { dropped: *dropped }) {
            Ok(()) => *dropped = 0,
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Closed(_)) => return false,
        }
    }
    match tx.try_send(event) {
        Ok(()) => {
            *delivered = true;
            true
        }
        Err(TrySendError::Full(_)) => {
            *dropped += 1;
            true
        }
        Err(TrySendError::Closed(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::json;

    fn decode_json(value: Value) -> StreamEvent {
        decode(&value.to_string(), SystemTime::UNIX_EPOCH).expect("JSON frame")
    }

    #[test]
    fn subscribe_message_is_the_untagged_first_message() {
        let msg = subscribe_message(&StreamChannel::all_for("BTC-USDX-PERP")).unwrap();
        let value: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(
            value,
            json!({ "subscribe": [
                "book:BTC-USDX-PERP",
                "trades:BTC-USDX-PERP",
                "market_status:BTC-USDX-PERP",
            ]})
        );
    }

    #[test]
    fn subscribe_message_dedups_and_accepts_the_wildcard() {
        let msg = subscribe_message(&[
            StreamChannel::book("*"),
            StreamChannel::trades("ETH-USDX-PERP"),
            StreamChannel::book("*"),
        ])
        .unwrap();
        assert_eq!(msg, r#"{"subscribe":["book:*","trades:ETH-USDX-PERP"]}"#);
    }

    #[test]
    fn subscribe_message_refuses_what_would_silently_match_nothing() {
        assert!(subscribe_message(&[]).is_err());
        assert!(subscribe_message(&[StreamChannel::book("")]).is_err());
        assert!(subscribe_message(&[StreamChannel::book("BTC USDX")]).is_err());
    }

    #[test]
    fn stream_url_is_the_rest_base_with_the_scheme_swapped() {
        assert_eq!(
            Network::Testnet.stream_url(),
            Some("wss://api.testnet.nexus.xyz/v1/stream")
        );
        assert_eq!(
            Network::Mainnet.stream_url(),
            Some("wss://api.nexus.xyz/v1/stream")
        );
        assert_eq!(
            Network::Local.stream_url(),
            Some("ws://localhost:9090/stream")
        );
        // Wherever a network declares the authenticated `/ws` socket, `/stream`
        // is its sibling on the same host and prefix.
        for network in [Network::Testnet, Network::Mainnet, Network::Local] {
            if let Some(ws) = network.ws_base() {
                if let Some(base) = ws.strip_suffix("/ws") {
                    assert_eq!(
                        network.stream_url(),
                        Some(format!("{base}/stream").as_str()),
                        "{network:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn book_update_parses_object_levels_exactly() {
        let event = decode_json(json!({
            "type": "BookUpdate",
            "market_id": "BTC-USDX-PERP",
            "book": {
                "bids": [{ "price": "64000.5", "quantity": "0.25", "order_count": 2 }],
                "asks": [{ "price": "64001.10", "quantity": "1", "order_count": 1 }],
                "sequence": 15995
            }
        }));
        let StreamEvent::Book(book) = event else {
            panic!("expected Book, got {event:?}");
        };
        assert_eq!(book.market_id, "BTC-USDX-PERP");
        assert_eq!(book.sequence, 15995);
        assert_eq!(
            book.bids,
            vec![BookLevel {
                price: dec!(64000.5),
                quantity: dec!(0.25),
                order_count: 2
            }]
        );
        assert_eq!(book.asks[0].price, dec!(64001.10));
        assert_eq!(book.received_at, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn empty_book_parses() {
        let event = decode_json(json!({
            "type": "BookUpdate", "market_id": "M",
            "book": { "bids": [], "asks": [], "sequence": 0 }
        }));
        assert!(matches!(event, StreamEvent::Book(b) if b.bids.is_empty() && b.asks.is_empty()));
    }

    #[test]
    fn every_other_frame_type_decodes() {
        let trade = decode_json(json!({
            "type": "Trade", "market_id": "BTC-USDX-PERP",
            "trade": {
                "id": "t1", "symbol": "BTC-USDX-PERP", "price": 64000.5, "amount": 0.1,
                "cost": 6400.05, "side": "buy", "timestamp": 1_700_000_000_000u64,
                "datetime": "2023-11-14T22:13:20Z", "takerOrMaker": "taker",
                "is_liquidation": false, "info": {}
            }
        }));
        assert!(matches!(trade, StreamEvent::Trade { ref trade, .. } if trade.id == "t1"));

        let halted = decode_json(json!({
            "type": "MarketHalted", "market_id": "M", "reason": "oracle", "timestamp": 5
        }));
        assert!(
            matches!(halted, StreamEvent::MarketHalted { ref reason, timestamp: 5, .. } if reason == "oracle")
        );

        let resumed =
            decode_json(json!({ "type": "MarketResumed", "market_id": "M", "timestamp": 6 }));
        assert!(matches!(
            resumed,
            StreamEvent::MarketResumed { timestamp: 6, .. }
        ));

        let gap = decode_json(json!({ "type": "gap", "missed": 42 }));
        assert!(matches!(gap, StreamEvent::Gap { missed: 42 }));
    }

    #[test]
    fn unknown_or_drifted_frames_are_passed_through() {
        let stats = decode_json(json!({ "type": "Stats", "stats": {} }));
        assert!(matches!(stats, StreamEvent::Unrecognized { .. }));
        // Withheld since ENG-17189; a server that predates it still sends it.
        let adl = decode_json(json!({
            "type": "AdlSettlement", "market_id": "M", "event": { "sequence": 7 }
        }));
        assert!(matches!(adl, StreamEvent::Unrecognized { .. }));
        // A level back in the REST pair shape is drift, not a silent empty book.
        let drifted = decode_json(json!({
            "type": "BookUpdate", "market_id": "M",
            "book": { "bids": [["1", "2"]], "asks": [], "sequence": 1 }
        }));
        assert!(matches!(drifted, StreamEvent::Unrecognized { .. }));
        assert!(decode("not json", SystemTime::now()).is_none());
    }
}
