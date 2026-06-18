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
//! use nexus_exchange::{Client, Config};
//! use serde_json::json;
//!
//! # async fn run() {
//! let client = Client::new(Config::default());
//! // Subscription frames are re-sent automatically after every reconnect.
//! let mut sub = client.connect(vec![json!({ "type": "subscribe", "channel": "trades" })]);
//! while let Some(event) = sub.next().await {
//!     // handle Connected / Disconnected / Message(..)
//!     let _ = event;
//! }
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
    /// Open a streaming connection and subscribe to the given frames.
    ///
    /// Returns immediately with a [`Subscription`]; connecting happens in a
    /// background task that emits [`Event::Connected`] once the socket is up.
    /// The task reconnects automatically with the configured [`Backoff`] and
    /// re-sends `subscriptions` after each reconnect.
    ///
    /// Must be called from within a Tokio runtime (it spawns a task).
    pub fn connect(&self, subscriptions: Vec<Value>) -> Subscription {
        let (event_tx, event_rx) = mpsc::channel(self.config.ws.channel_capacity);
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let ws_url = self.config.ws_url.clone();
        let backoff = self.config.ws.backoff.clone();

        let handle = tokio::spawn(run(ws_url, backoff, subscriptions, event_tx, cmd_rx));

        Subscription {
            rx: event_rx,
            cmd_tx,
            handle,
        }
    }
}

/// The background reconnect loop: connect, run until the socket drops or a
/// command stops it, then back off and try again.
async fn run(
    ws_url: String,
    backoff: Backoff,
    mut subscriptions: Vec<Value>,
    event_tx: mpsc::Sender<Event>,
    mut cmd_rx: mpsc::Receiver<Command>,
) {
    let mut delays = backoff.iter();

    loop {
        match tokio_tungstenite::connect_async(ws_url.as_str()).await {
            Ok((stream, _resp)) => {
                if event_tx.send(Event::Connected).await.is_err() {
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

                // Only treat the connection as healthy — and reset the backoff so
                // the next outage retries promptly — once it actually carried a
                // message. A socket that completes the handshake and immediately
                // drops (auth-reject-after-upgrade, LB flap) must keep backing off
                // rather than reset every cycle into a tight reconnect loop.
                if delivered {
                    delays.reset();
                }

                match exit {
                    LoopExit::Closed | LoopExit::ConsumerGone => return,
                    LoopExit::Reconnect(reason) => {
                        if event_tx.send(Event::Disconnected(reason)).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Err(err) => {
                if event_tx
                    .send(Event::Disconnected(format!("connect failed: {err}")))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }

        // Back off before retrying. A Close command cuts the wait short; a
        // Subscribe is queued for the next connect but must *not* shorten the
        // backoff — otherwise a client subscribing while the endpoint is down
        // would defeat the backoff and hammer it. So we hold a fixed deadline
        // and keep waiting out the remainder after queueing each frame.
        let deadline = tokio::time::Instant::now() + delays.next_delay();
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                cmd = cmd_rx.recv() => match cmd {
                    Some(Command::Close) | None => return,
                    Some(Command::Subscribe(req)) => subscriptions.push(req),
                }
            }
        }
    }
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
        // already gone; reconnect.
        Message::Ping(payload) => match write.send(Message::Pong(payload)).await {
            Ok(()) => None,
            Err(_) => Some(LoopExit::Reconnect("pong send failed".to_string())),
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
