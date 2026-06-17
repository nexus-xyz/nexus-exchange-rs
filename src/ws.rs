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
//!   behind, the read loop blocks on the channel instead of buffering frames
//!   forever — backpressure that bounds memory at the cost of pausing reads.
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
                // A clean connection resets the backoff so the *next* outage
                // starts from the initial delay rather than wherever we left off.
                delays.reset();
                if event_tx.send(Event::Connected).await.is_err() {
                    return; // consumer gone
                }

                match serve(stream, &event_tx, &mut cmd_rx, &mut subscriptions).await {
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

        // Back off before retrying, but let a Close command cut the wait short.
        let delay = delays.next_delay();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Close) | None => return,
                // Queue the subscription so it's sent once we reconnect.
                Some(Command::Subscribe(req)) => subscriptions.push(req),
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

    loop {
        tokio::select! {
            // Incoming frames from the server.
            frame = read.next() => match frame {
                Some(Ok(msg)) => {
                    if let Some(exit) = handle_message(msg, event_tx, &mut write).await {
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
/// Note: delivering a `Message` may block on the bounded channel when the
/// consumer is behind. That is the backpressure path — while blocked we are
/// not reading further frames (including pings), which is the intended
/// trade-off of a bounded queue.
async fn handle_message<W>(
    msg: Message,
    event_tx: &mpsc::Sender<Event>,
    write: &mut W,
) -> Option<LoopExit>
where
    W: SinkExt<Message> + Unpin,
{
    match msg {
        Message::Text(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => deliver(event_tx, value).await,
            // A non-JSON text frame on a JSON feed: skip it rather than tear
            // down an otherwise healthy connection.
            Err(_) => None,
        },
        Message::Binary(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => deliver(event_tx, value).await,
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

/// Forward a parsed message to the consumer, signalling shutdown if the
/// consumer has dropped its receiver.
async fn deliver(event_tx: &mpsc::Sender<Event>, value: Value) -> Option<LoopExit> {
    match event_tx.send(Event::Message(value)).await {
        Ok(()) => None,
        Err(_) => Some(LoopExit::ConsumerGone),
    }
}
