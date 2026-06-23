//! WebSocket streaming client (`/ws`).
//!
//! Two failure modes sink most exchange WebSocket clients (Hyperliquid and
//! dYdX integrations both hit them): a *fixed-sleep* reconnect that stampedes
//! the endpoint the moment it recovers, and an *unbounded* internal queue that
//! grows without limit when the consumer can't keep up — trading OOM for the
//! disconnect it was trying to avoid. This client closes both:
//!
//! * **Reconnect** uses capped exponential backoff with full jitter
//!   ([`Backoff`]), so a fleet of clients spreads its retries instead of
//!   synchronizing on a fixed interval.
//! * **Delivery** flows through a *bounded* channel. When the consumer falls
//!   behind and the channel fills, the read loop drops the excess frames rather
//!   than blocking — bounding memory *and* keeping the socket drained so server
//!   pings are always read and ponged. Dropped frames are reported, not silent.
//!
//! # Delivery guarantees
//!
//! Within a single connection, delivery is **order-preserving and gap-aware**:
//! frames arrive in server order with no duplicates, and as long as the consumer
//! keeps up it is lossless. If the consumer falls behind, excess frames are
//! dropped to keep the socket drained, and the number dropped is reported as an
//! [`Event::Lagged`] immediately before the next delivered message — so a
//! consumer always knows when and how much it missed (similar to a lagging
//! broadcast receiver). Size [`channel_capacity`] for the consumer's worst-case
//! burst to avoid drops.
//!
//! This non-blocking read is deliberate: an earlier design blocked the read loop
//! on a full channel, which let a slow consumer starve the keepalive Pong (the
//! Ping sits unread behind buffered data) and lose the connection. Dropping with
//! an explicit `Lagged` signal keeps keepalive independent of consumer speed.
//!
//! None of this extends across reconnects — the client replays subscription
//! frames verbatim but carries no sequence/cursor, so events the server emitted
//! between the drop and the resubscribe are missed, and any snapshot the server
//! re-sends on (re)subscribe arrives as a duplicate. A consumer that needs
//! gap-free streams across reconnects must dedup/resume itself until cursor-based
//! resume lands (tracked separately).
//!
//! [`channel_capacity`]: crate::Config::with_channel_capacity
//!
//! # Example
//!
//! ```no_run
//! use nexus_exchange::{Client, Config, Network};
//! use serde_json::json;
//!
//! # async fn run() -> nexus_exchange::Result<()> {
//! // Public streams. The WS host is a separate origin from the REST base, so
//! // it must be known for the network (or set with `Config::with_ws_url`).
//! let client = Client::new(Config::new(Network::Local));
//! // Subscription frames are re-sent automatically after every reconnect.
//! let mut sub = client.connect(vec![json!({ "type": "subscribe", "channel": "trades" })]);
//! while let Some(event) = sub.next().await {
//!     // handle Connected / Disconnected / Message / Lagged
//!     let _ = event;
//! }
//!
//! // Authenticated streams: mint a single-use token and connect in one step.
//! let client = Client::new(Config::new(Network::Local).api_key("key-id", "00ff..."));
//! let mut sub = client
//!     .connect_ws(vec![json!({ "type": "subscribe", "channel": "fills" })])
//!     .await?;
//! while let Some(event) = sub.next().await {
//!     let _ = event;
//! }
//! # Ok(())
//! # }
//! ```

mod backoff;

pub use backoff::{Backoff, BackoffIter};

use crate::{Client, Error, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// Capacity of the (rarely-used) command channel from a [`Subscription`] to
/// its background task.
const COMMAND_CHANNEL_CAPACITY: usize = 16;

/// An event delivered by a [`Subscription`].
///
/// `Connected` / `Disconnected` bracket each underlying socket lifetime, so a
/// consumer can observe reconnects; the actual payloads arrive as `Message`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// A socket connection was (re)established and subscriptions were re-sent.
    Connected,
    /// The socket dropped; the client is now backing off before reconnecting.
    /// Carries a human-readable reason.
    Disconnected(String),
    /// A JSON message frame from the server.
    Message(Value),
    /// The consumer fell behind and `dropped` message frames were discarded to
    /// keep reading the socket (so keepalive Pongs are never starved). Emitted
    /// in order, immediately before the next delivered [`Message`](Self::Message),
    /// so a consumer can detect the gap. See the module-level delivery
    /// guarantees.
    Lagged {
        /// Number of message frames dropped since the last delivered message.
        dropped: u64,
    },
}

/// A command sent from a [`Subscription`] handle to its background task.
#[derive(Debug)]
enum Command {
    /// Add a subscription frame: send it now (if connected) and on every
    /// future reconnect.
    Subscribe(Value),
    /// Close the connection and stop the task.
    Close,
}

/// Why the inner connection loop returned.
enum LoopExit {
    /// Socket failed; reconnect after backoff. Carries the reason.
    Reconnect(String),
    /// A graceful close was requested; stop entirely.
    Closed,
    /// The consumer dropped its [`Subscription`]; stop entirely.
    ConsumerGone,
}

/// A live subscription to the streaming API.
///
/// Pull events with [`next`](Self::next). The background task owns the socket,
/// reconnects transparently with backoff, and re-sends every subscription
/// frame after reconnecting. Dropping the `Subscription` (or calling
/// [`close`](Self::close)) shuts the task down.
#[derive(Debug)]
pub struct Subscription {
    rx: mpsc::Receiver<Event>,
    cmd_tx: mpsc::Sender<Command>,
    handle: JoinHandle<()>,
}

impl Subscription {
    /// Await the next [`Event`], or `None` once the stream has shut down (the
    /// background task ended and its channel drained).
    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    /// Register an additional subscription frame. It is sent immediately if a
    /// connection is up and re-sent on every subsequent reconnect.
    ///
    /// This is **additive and not deduplicated**: every frame passed here (and
    /// to [`Client::connect`]) is retained and replayed verbatim on each
    /// reconnect, and there is no unsubscribe yet. Callers with a churn-y
    /// subscription set should track membership themselves rather than relying
    /// on repeated `subscribe` calls, which would accumulate replayed frames.
    pub async fn subscribe(&self, request: Value) -> Result<()> {
        self.cmd_tx
            .send(Command::Subscribe(request))
            .await
            .map_err(|_| Error::StreamClosed)
    }

    /// Gracefully close the connection and wait for the background task to end.
    pub async fn close(self) {
        // Best-effort: if the task already stopped, the send just fails.
        let _ = self.cmd_tx.send(Command::Close).await;
        let _ = self.handle.await;
    }
}

impl Client {
    /// Open an **unauthenticated** streaming connection and subscribe to the
    /// given frames.
    ///
    /// Returns immediately with a [`Subscription`]; connecting happens in a
    /// background task that emits [`Event::Connected`] once the socket is up.
    /// The task reconnects automatically with the configured [`Backoff`] and
    /// re-sends `subscriptions` after each reconnect.
    ///
    /// The stream targets the configured WebSocket origin (a separate host from
    /// the REST base — see [`Network::ws_base`](crate::Network::ws_base)). If
    /// none is configured for the network (production host unconfirmed —
    /// ENG-3398) the background task immediately emits a single
    /// [`Event::Disconnected`] explaining that and stops; set one with
    /// [`Config::with_ws_url`](crate::Config::with_ws_url) or use a network
    /// whose host is known.
    ///
    /// Must be called from within a Tokio runtime (it spawns a task).
    pub fn connect(&self, subscriptions: Vec<Value>) -> Subscription {
        self.spawn_ws(None, subscriptions)
    }

    /// Mint a single-use WebSocket token and open an **authenticated** stream
    /// in one step — the convenience the lower-level [`connect`](Self::connect)
    /// plus [`mint_web_socket_token`](Self::mint_web_socket_token) would
    /// otherwise require callers to wire up themselves.
    ///
    /// Requires credentials (the token mint is signed). Fails fast — before any
    /// network round-trip — if no WebSocket endpoint is configured for the
    /// network (production host unconfirmed — ENG-3398); set one with
    /// [`Config::with_ws_url`](crate::Config::with_ws_url) until it is.
    ///
    /// The minted token is short-lived and **single-use**, so it authenticates
    /// exactly one connection. The background task therefore mints a *fresh*
    /// token before every reconnect rather than replaying the spent one; a mint
    /// that fails surfaces as an [`Event::Disconnected`] and is retried under
    /// the same backoff as a failed connect. The token is presented as a query
    /// parameter and is never written to an [`Event`] or logged.
    ///
    /// Must be called from within a Tokio runtime (it spawns a task).
    pub async fn connect_ws(&self, subscriptions: Vec<Value>) -> Result<Subscription> {
        // Resolve the endpoint first: an unconfigured network must fail here,
        // not after spending a mint round-trip on a stream that can't connect.
        if self.config.ws_url.is_none() {
            return Err(Error::InvalidRequest(
                "no WebSocket endpoint configured for this network (production WS host \
                 not yet confirmed — ENG-3398); set one with Config::with_ws_url"
                    .to_string(),
            ));
        }
        // Pre-mint the first token so credential / transport problems surface
        // here as an error rather than as a background Disconnected event.
        let token = self.mint_web_socket_token().await?.token;
        Ok(self.spawn_ws(Some(token), subscriptions))
    }

    /// Spawn the streaming task. `first_token` is `Some` for an authenticated
    /// stream — the pre-minted token used for the first connect, after which
    /// the task re-mints — and `None` for an unauthenticated one.
    fn spawn_ws(&self, first_token: Option<String>, subscriptions: Vec<Value>) -> Subscription {
        let (event_tx, event_rx) = mpsc::channel(self.config.ws.channel_capacity);
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let base = self.config.ws_url.clone();
        let backoff = self.config.ws.backoff.clone();
        // Carry a client clone to re-mint tokens on reconnect, but only for an
        // authenticated stream — an unauthenticated one never mints.
        let auth = first_token.as_ref().map(|_| self.clone());

        let handle = tokio::spawn(run(
            base,
            auth,
            first_token,
            backoff,
            subscriptions,
            event_tx,
            cmd_rx,
        ));

        Subscription {
            rx: event_rx,
            cmd_tx,
            handle,
        }
    }
}

/// Emit a lifecycle event ([`Event::Connected`] / [`Event::Disconnected`])
/// without blocking the task.
///
/// Like data frames, lifecycle markers go out via a non-blocking `try_send`: a
/// consumer that left the channel full must never stall the task — in
/// particular at the reconnect boundary, where a blocked `Connected` would
/// delay the read loop and with it the keepalive Pong. A full channel drops the
/// marker (the consumer is already behind and will see [`Event::Lagged`]).
/// Returns `false` only once the consumer is gone, so the caller can stop.
fn emit_lifecycle(event_tx: &mpsc::Sender<Event>, event: Event) -> bool {
    !matches!(event_tx.try_send(event), Err(TrySendError::Closed(_)))
}

/// The background reconnect loop: (re-mint a token,) connect, run until the
/// socket drops or a command stops it, then back off and try again.
///
/// `base` is the WebSocket origin; `None` means the network has no known WS
/// host (production unconfirmed — ENG-3398), reported once before stopping.
/// When `auth` is `Some`, every connect is authenticated with a single-use
/// token: `first_token` (pre-minted by [`Client::connect_ws`]) is used for the
/// first attempt and a fresh one is minted for each reconnect.
async fn run(
    base: Option<String>,
    auth: Option<Client>,
    mut first_token: Option<String>,
    backoff: Backoff,
    mut subscriptions: Vec<Value>,
    event_tx: mpsc::Sender<Event>,
    mut cmd_rx: mpsc::Receiver<Command>,
) {
    let Some(base) = base else {
        // `connect` on a network with no known WS host: report once and stop,
        // rather than spin a backoff loop against an endpoint that can't exist.
        let _ = emit_lifecycle(
            &event_tx,
            Event::Disconnected(
                "no WebSocket endpoint configured for this network (production WS host \
                 not yet confirmed — ENG-3398); set one with Config::with_ws_url"
                    .to_string(),
            ),
        );
        return;
    };

    let mut delays = backoff.iter();

    loop {
        // Resolve this attempt's connect URL. Authenticated streams mint a
        // fresh single-use token per (re)connect — reusing the spent one would
        // be rejected — using the pre-minted token only for the first attempt.
        let url = match &auth {
            None => Ok(base.clone()),
            Some(client) => match first_token.take() {
                Some(token) => Ok(ws_url_with_token(&base, &token)),
                None => match client.mint_web_socket_token().await {
                    Ok(minted) => Ok(ws_url_with_token(&base, &minted.token)),
                    Err(err) => Err(format!("ws token mint failed: {err}")),
                },
            },
        };

        match url {
            Err(reason) => {
                // Mint failed: report (token redaction is a no-op here, the
                // mint request carries none) and back off like a failed connect.
                if !emit_lifecycle(&event_tx, Event::Disconnected(redact_token(reason))) {
                    return;
                }
            }
            Ok(url) => match tokio_tungstenite::connect_async(url.as_str()).await {
                Ok((stream, _resp)) => {
                    if !emit_lifecycle(&event_tx, Event::Connected) {
                        return; // consumer gone
                    }

                    let mut delivered = false;
                    let exit = serve(
                        stream,
                        &event_tx,
                        &mut cmd_rx,
                        &mut subscriptions,
                        &mut delivered,
                    )
                    .await;

                    // Only treat the connection as healthy — and reset the backoff
                    // so the next outage retries promptly — once it actually carried
                    // a message. A socket that completes the handshake and immediately
                    // drops (auth-reject-after-upgrade, LB flap) must keep backing off
                    // rather than reset every cycle into a tight reconnect loop.
                    if delivered {
                        delays.reset();
                    }

                    match exit {
                        LoopExit::Closed | LoopExit::ConsumerGone => return,
                        LoopExit::Reconnect(reason) => {
                            if !emit_lifecycle(&event_tx, Event::Disconnected(redact_token(reason)))
                            {
                                return;
                            }
                        }
                    }
                }
                Err(err) => {
                    // Redact defensively: the token rides in `url`'s query, and
                    // a transport error must never echo it into an event/log.
                    if !emit_lifecycle(
                        &event_tx,
                        Event::Disconnected(redact_token(format!("connect failed: {err}"))),
                    ) {
                        return;
                    }
                }
            },
        }

        // Back off before retrying. A Close command cuts the wait short; a
        // Subscribe is queued for the next connect but must *not* shorten the
        // backoff — otherwise a client subscribing while the endpoint is down
        // would defeat the backoff and hammer it.
        if wait_backoff(&mut delays, &mut cmd_rx, &mut subscriptions).await {
            return;
        }
    }
}

/// Wait out one backoff delay before the next (re)connect, holding a fixed
/// deadline so queued `Subscribe` frames don't shorten it. Returns `true` when
/// a `Close` command (or a dropped handle) means the task should stop.
async fn wait_backoff(
    delays: &mut BackoffIter,
    cmd_rx: &mut mpsc::Receiver<Command>,
    subscriptions: &mut Vec<Value>,
) -> bool {
    let deadline = tokio::time::Instant::now() + delays.next_delay();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Close) | None => return true,
                Some(Command::Subscribe(req)) => subscriptions.push(req),
            }
        }
    }
}

/// Append the single-use auth token to the WS URL as a query parameter.
///
/// Tokens minted for browser clients are presented this way — the WebSocket
/// upgrade can't carry custom headers — and the value is percent-encoded via
/// `serde_urlencoded` so it can't break out of the query.
fn ws_url_with_token(base: &str, token: &str) -> String {
    // Encoding a single static-key string pair cannot fail; `expect` documents
    // that and refuses to connect rather than silently dropping the token (and
    // connecting unauthenticated) on an empty encode.
    let pair = serde_urlencoded::to_string([("token", token)])
        .expect("encoding a single string query pair is infallible");
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}{pair}")
}

/// Strip every `token=<value>` query fragment from a human-readable reason so a
/// single-use WS token can never leak into an [`Event`] or a log, even if an
/// underlying error were to echo the connect URL. Defense in depth: today's
/// transport errors carry the socket address, not the query.
fn redact_token(reason: String) -> String {
    const KEY: &str = "token=";
    if !reason.contains(KEY) {
        return reason;
    }
    let mut out = String::with_capacity(reason.len());
    let mut rest = reason.as_str();
    // Scrub all occurrences, not just the first, in case a reason ever echoes
    // the URL more than once.
    while let Some(start) = rest.find(KEY) {
        let val_start = start + KEY.len();
        // The value is percent-encoded, so it ends at the first delimiter.
        let val_len = rest[val_start..]
            .find(['&', ' ', '"', ')'])
            .unwrap_or(rest.len() - val_start);
        out.push_str(&rest[..val_start]);
        out.push_str("***");
        rest = &rest[val_start + val_len..];
    }
    out.push_str(rest);
    out
}

/// Drive a single live connection until it drops or the task is told to stop.
async fn serve<S>(
    stream: tokio_tungstenite::WebSocketStream<S>,
    event_tx: &mpsc::Sender<Event>,
    cmd_rx: &mut mpsc::Receiver<Command>,
    subscriptions: &mut Vec<Value>,
    delivered: &mut bool,
) -> LoopExit
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut write, mut read) = stream.split();

    // (Re)send every known subscription frame for this fresh connection.
    for req in subscriptions.iter() {
        if let Err(err) = write.send(Message::Text(req.to_string().into())).await {
            return LoopExit::Reconnect(format!("subscribe send failed: {err}"));
        }
    }

    // Count of message frames dropped while the consumer was behind; reported to
    // the consumer as an `Event::Lagged` before the next delivered message.
    let mut dropped: u64 = 0;

    loop {
        tokio::select! {
            // Incoming frames from the server.
            frame = read.next() => match frame {
                Some(Ok(msg)) => {
                    if let Some(exit) =
                        handle_message(msg, event_tx, &mut write, delivered, &mut dropped).await
                    {
                        return exit;
                    }
                }
                Some(Err(err)) => return LoopExit::Reconnect(format!("read error: {err}")),
                None => return LoopExit::Reconnect("stream ended".to_string()),
            },

            // Commands from the Subscription handle.
            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Subscribe(req)) => {
                    if let Err(err) = write.send(Message::Text(req.to_string().into())).await {
                        // Re-queue so the resubscribe happens after reconnect.
                        subscriptions.push(req);
                        return LoopExit::Reconnect(format!("subscribe send failed: {err}"));
                    }
                    subscriptions.push(req);
                }
                Some(Command::Close) => {
                    let _ = write.send(Message::Close(None)).await;
                    return LoopExit::Closed;
                }
                None => return LoopExit::ConsumerGone, // handle dropped
            },
        }
    }
}

/// Process one inbound frame. Returns `Some(exit)` to end the connection loop,
/// or `None` to keep going.
///
/// Delivering a `Message` never blocks the read loop: it uses a non-blocking
/// `try_send`, and when the bounded channel is full it drops the frame and
/// counts it (surfaced to the consumer as [`Event::Lagged`]). This keeps the
/// loop reading — so server pings are always read and ponged promptly — at the
/// cost of dropping data for a consumer that can't keep up. See the
/// module-level delivery guarantees.
async fn handle_message<W>(
    msg: Message,
    event_tx: &mpsc::Sender<Event>,
    write: &mut W,
    delivered: &mut bool,
    dropped: &mut u64,
) -> Option<LoopExit>
where
    W: SinkExt<Message> + Unpin,
    <W as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    match msg {
        Message::Text(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => deliver(event_tx, value, delivered, dropped),
            // A non-JSON text frame on a JSON feed: skip it rather than tear
            // down an otherwise healthy connection.
            Err(_) => None,
        },
        Message::Binary(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => deliver(event_tx, value, delivered, dropped),
            Err(_) => None,
        },
        // Keep the connection alive. If the pong can't be sent the socket is
        // already gone; reconnect, surfacing the underlying error like the
        // read/connect paths do.
        Message::Ping(payload) => match write.send(Message::Pong(payload)).await {
            Ok(()) => None,
            Err(err) => Some(LoopExit::Reconnect(format!("pong send failed: {err}"))),
        },
        Message::Close(frame) => {
            let reason = frame
                .map(|f| format!("server closed: {} {}", f.code, f.reason))
                .unwrap_or_else(|| "server closed".to_string());
            Some(LoopExit::Reconnect(reason))
        }
        // Pong / raw Frame: nothing to do.
        _ => None,
    }
}

/// Forward a parsed message to the consumer without blocking the read loop.
///
/// On a full channel the frame is dropped and counted in `dropped`; the count
/// is flushed as an [`Event::Lagged`] immediately before the next successfully
/// delivered message, so the consumer sees gaps in order. Returns
/// [`LoopExit::ConsumerGone`] once the receiver has been dropped. Sets
/// `delivered` on the first hand-off, marking the connection healthy enough to
/// reset the backoff.
fn deliver(
    event_tx: &mpsc::Sender<Event>,
    value: Value,
    delivered: &mut bool,
    dropped: &mut u64,
) -> Option<LoopExit> {
    // Flush a pending lag marker first so the consumer learns about the gap
    // ahead of (and in order with) the message that follows it.
    if *dropped > 0 {
        match event_tx.try_send(Event::Lagged { dropped: *dropped }) {
            Ok(()) => *dropped = 0,
            // Still no room — keep accumulating; report once the consumer drains.
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Closed(_)) => return Some(LoopExit::ConsumerGone),
        }
    }

    match event_tx.try_send(Event::Message(value)) {
        Ok(()) => {
            *delivered = true;
            None
        }
        // Consumer is behind: drop this frame and count it for the next Lagged.
        Err(TrySendError::Full(_)) => {
            *dropped += 1;
            None
        }
        Err(TrySendError::Closed(_)) => Some(LoopExit::ConsumerGone),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ws_url_with_token_appends_encoded_query() {
        // No existing query: starts one.
        assert_eq!(
            ws_url_with_token("wss://ws.example/ws", "ab cd/+="),
            "wss://ws.example/ws?token=ab+cd%2F%2B%3D"
        );
        // Existing query: appends with `&`.
        assert_eq!(
            ws_url_with_token("wss://ws.example/ws?v=2", "tok"),
            "wss://ws.example/ws?v=2&token=tok"
        );
    }

    #[test]
    fn redact_token_hides_the_value_only() {
        assert_eq!(
            redact_token("connect failed: error connecting to wss://h/ws?token=secret123".into()),
            "connect failed: error connecting to wss://h/ws?token=***"
        );
        // Value ends at a delimiter, leaving the rest intact.
        assert_eq!(
            redact_token("url wss://h/ws?token=secret&x=1 refused".into()),
            "url wss://h/ws?token=***&x=1 refused"
        );
        // Every occurrence is scrubbed, not just the first.
        assert_eq!(
            redact_token("token=a&x=1 then token=b end".into()),
            "token=***&x=1 then token=*** end"
        );
        // Nothing to redact is returned unchanged.
        assert_eq!(redact_token("read error: eof".into()), "read error: eof");
    }

    /// Drive `deliver` against a tiny channel, draining at controlled points so
    /// we exercise both the drop path (channel full) and the lag-report path
    /// (a flushed `Lagged` marker), then prove the delivered stream is
    /// order-preserving, duplicate-free, and accounts for every frame exactly
    /// once (delivered, reported as lagged, or still pending).
    #[test]
    fn deliver_drops_excess_and_reports_via_lagged() {
        const TOTAL: u64 = 7;
        let (tx, mut rx) = mpsc::channel::<Event>(2);
        let mut delivered = false;
        let mut dropped = 0u64;
        let mut events: Vec<Event> = Vec::new();

        for seq in 0..TOTAL {
            // Free the channel before these sends so a pending Lagged can flush.
            if seq == 4 || seq == 6 {
                while let Ok(ev) = rx.try_recv() {
                    events.push(ev);
                }
            }
            let exit = deliver(&tx, json!({ "seq": seq }), &mut delivered, &mut dropped);
            assert!(exit.is_none(), "consumer is present, no exit expected");
        }
        drop(tx);
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        // Reconstruct: every Message's seq must equal the running count of
        // (delivered + reported-dropped), so any reorder or duplicate trips here.
        let mut expected = 0u64;
        let mut received = 0u64;
        let mut lagged_total = 0u64;
        for ev in events {
            match ev {
                Event::Lagged { dropped } => {
                    assert!(dropped > 0, "Lagged must report a positive gap");
                    expected += dropped;
                    lagged_total += dropped;
                }
                Event::Message(v) => {
                    assert_eq!(
                        v["seq"].as_u64().unwrap(),
                        expected,
                        "reorder/duplicate frame"
                    );
                    expected += 1;
                    received += 1;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }

        assert_eq!(
            received + lagged_total + dropped,
            TOTAL,
            "every frame is delivered, reported as lagged, or still pending"
        );
        assert!(
            delivered,
            "the first frame is delivered before the channel fills"
        );
        assert!(lagged_total > 0, "a Lagged marker should have been flushed");
        assert!(received < TOTAL, "capacity 2 vs 7 frames must drop some");
    }
}
