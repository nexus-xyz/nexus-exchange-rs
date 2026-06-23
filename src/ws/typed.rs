//! Typed, protocol-aware streaming client.
//!
//! [`Client::subscribe`] returns a [`MessageStream`] — a typed
//! [`Stream`](futures_core::Stream) of decoded [`ServerMessage`]s for the
//! op-envelope protocol (see [`crate::ws::protocol`]). It builds on the same
//! reliability guarantees as the raw [`Subscription`](crate::ws::Subscription)
//! client — capped exponential backoff with jitter and a bounded, non-blocking
//! delivery channel — and adds two things on top:
//!
//! * **Token upgrade.** Account channels are private, so before each connection
//!   the client mints a fresh single-use `/ws/token` and presents it on the
//!   upgrade URL. Tokens are single-use, so a new one is minted on *every*
//!   reconnect, not just the first connect.
//! * **Cursor resume.** The client tracks the highest `seq` seen per channel
//!   (seeded from each `subscribed` frame's `seq_at_join`) and, on reconnect,
//!   replays each `subscribe` with a `since` cursor so the server resumes after
//!   the last frame the client processed.
//!
//! # Delivery semantics
//!
//! The stream yields `Result<ServerMessage, Error>`:
//!
//! * `Ok(msg)` — a decoded protocol frame, in server order within a connection.
//! * `Err(`[`Error::Lagged`]`)` — the consumer fell behind and `dropped` frames
//!   were discarded to keep the socket drained (so keepalive pongs are never
//!   starved). Emitted in order, immediately before the next `Ok`. The stream
//!   continues; this is a gap signal, not a fatal error.
//! * `Err(other)` — a `/ws/token` mint failed (often an auth problem). The
//!   client still backs off and retries, so a transient mint failure recovers;
//!   a persistent one keeps surfacing, paced by the backoff.
//!
//! Transient connect / read failures are **not** surfaced: the client reconnects
//! transparently and the consumer simply sees the resumed stream (a fresh
//! `subscribed` frame per channel marks each reconnect). The stream ends
//! (`None`) on a graceful [`close`](MessageStream::close) or once the handle is
//! dropped.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::ws::protocol::{Channel, CursorKey, ServerMessage};
use crate::ws::Backoff;
use crate::{Client, Error, Result};

/// Capacity of the (rarely-used) command channel from a [`MessageStream`] to its
/// background task.
const COMMAND_CHANNEL_CAPACITY: usize = 16;

/// The item type delivered by a [`MessageStream`].
type Item = Result<ServerMessage>;

/// A command sent from a [`MessageStream`] handle to its background task.
#[derive(Debug)]
enum Command {
    /// Add a channel: subscribe now (if connected) and on every reconnect.
    Subscribe(Channel),
    /// Remove a channel: unsubscribe now and stop replaying it.
    Unsubscribe(Channel),
    /// Close the connection and stop the task.
    Close,
}

/// Why the inner connection loop returned.
enum LoopExit {
    /// Socket failed or a re-auth is needed; reconnect after backoff.
    Reconnect,
    /// A graceful close was requested; stop entirely.
    Closed,
    /// The consumer dropped its [`MessageStream`]; stop entirely.
    ConsumerGone,
}

/// A live, typed subscription to the streaming API.
///
/// Created by [`Client::subscribe`]. Implements [`Stream`](futures_core::Stream)
/// with item type `Result<`[`ServerMessage`]`, `[`Error`]`>`, so it composes
/// with [`futures_util::StreamExt`] (`.next()`, `.filter_map()`, …). The
/// background task owns the socket, reconnects transparently with backoff,
/// re-mints the `/ws/token` for private streams, and resumes each channel from
/// its `since` cursor. Dropping the handle (or calling [`close`](Self::close))
/// shuts the task down.
#[derive(Debug)]
pub struct MessageStream {
    rx: mpsc::Receiver<Item>,
    cmd_tx: mpsc::Sender<Command>,
    /// `Some` until [`close`](Self::close) takes it; `Drop` aborts whatever
    /// remains so the task can never outlive the handle.
    handle: Option<JoinHandle<()>>,
}

impl Stream for MessageStream {
    type Item = Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl MessageStream {
    /// Add a channel to the subscription. It is sent immediately if a connection
    /// is up and replayed on every subsequent reconnect. Subscribing to a
    /// channel already in the set is a no-op.
    ///
    /// Subscribing to a private channel on a currently-public connection
    /// triggers a transparent reconnect so the client can mint a token and
    /// upgrade.
    pub async fn subscribe(&self, channel: Channel) -> Result<()> {
        self.cmd_tx
            .send(Command::Subscribe(channel))
            .await
            .map_err(|_| Error::StreamClosed)
    }

    /// Remove a channel from the subscription: unsubscribe on the wire and stop
    /// replaying it on reconnect.
    pub async fn unsubscribe(&self, channel: Channel) -> Result<()> {
        self.cmd_tx
            .send(Command::Unsubscribe(channel))
            .await
            .map_err(|_| Error::StreamClosed)
    }

    /// Gracefully close the connection and wait for the background task to end.
    pub async fn close(mut self) {
        // Best-effort: if the task already stopped, the send just fails.
        let _ = self.cmd_tx.send(Command::Close).await;
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for MessageStream {
    fn drop(&mut self) {
        // If the handle wasn't consumed by `close`, abort the task so it never
        // outlives its consumer (e.g. while blocked minting a token or
        // connecting). Aborting an already-finished task is a no-op.
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

impl Client {
    /// Open a typed streaming connection subscribed to `channels`.
    ///
    /// Returns immediately with a [`MessageStream`]; connecting happens in a
    /// background task. If any channel [`is_private`](Channel::is_private), the
    /// client mints a single-use `/ws/token` before each connection and presents
    /// it on the upgrade URL — so this requires credentials, and a request for a
    /// private channel without them fails fast here with [`Error::Auth`].
    ///
    /// Must be called from within a Tokio runtime (it spawns a task).
    ///
    /// ```no_run
    /// use futures_util::StreamExt;
    /// use nexus_exchange::ws::Channel;
    /// use nexus_exchange::{Client, Config};
    ///
    /// # async fn run() -> nexus_exchange::Result<()> {
    /// let client = Client::new(Config::default());
    /// let mut stream = client.subscribe(vec![Channel::trades("BTC-USDX-PERP")])?;
    /// while let Some(item) = stream.next().await {
    ///     match item {
    ///         Ok(msg) => { let _ = msg; /* handle the typed frame */ }
    ///         Err(err) => eprintln!("stream: {err}"),
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn subscribe(&self, channels: Vec<Channel>) -> Result<MessageStream> {
        // Fail fast: a private channel without credentials can never authenticate.
        if channels.iter().any(Channel::is_private) && self.config.credentials.is_none() {
            return Err(Error::Auth(
                "private channels require credentials to mint a /ws/token".into(),
            ));
        }

        // The WS origin is a separate host from the REST base and isn't known
        // for every network (production host unconfirmed — ENG-3398); fail fast
        // rather than spawn a task that can't connect.
        let ws_url = self.config.ws_url.clone().ok_or_else(|| {
            Error::InvalidRequest(
                "no WebSocket endpoint configured for this network (production WS host \
                 not yet confirmed — ENG-3398); set one with Config::with_ws_url"
                    .to_string(),
            )
        })?;

        let (event_tx, event_rx) = mpsc::channel(self.config.ws.channel_capacity);
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let handle = tokio::spawn(run(
            self.clone(),
            ws_url,
            self.config.ws.backoff.clone(),
            channels,
            event_tx,
            cmd_rx,
        ));

        Ok(MessageStream {
            rx: event_rx,
            cmd_tx,
            handle: Some(handle),
        })
    }
}

/// The background reconnect loop: resolve the (possibly token-bearing) URL,
/// connect, run until the socket drops or a command stops it, then back off and
/// try again — resuming each channel from its cursor.
async fn run(
    client: Client,
    ws_url: String,
    backoff: Backoff,
    mut channels: Vec<Channel>,
    event_tx: mpsc::Sender<Item>,
    mut cmd_rx: mpsc::Receiver<Command>,
) {
    let mut delays = backoff.iter();
    // Highest `seq` processed per channel — the resume cursors.
    let mut cursors: HashMap<CursorKey, u64> = HashMap::new();

    loop {
        let authed = channels.iter().any(Channel::is_private);

        // Mint a fresh single-use token for private streams and build the URL.
        let url = match connect_url(&client, &ws_url, authed).await {
            Ok(url) => url,
            Err(err) => {
                // Surface mint failures (often auth) but keep retrying so a
                // transient one recovers; the backoff paces a persistent one.
                if !emit(&event_tx, Err(err)) {
                    return; // consumer gone
                }
                if backoff_wait(&mut delays, &mut cmd_rx, &mut channels, &mut cursors).await {
                    return;
                }
                continue;
            }
        };

        // A connect failure is transient and transparent; fall through to back
        // off and retry. On success, serve the connection until it drops.
        if let Ok((stream, _resp)) = tokio_tungstenite::connect_async(url.as_str()).await {
            let mut delivered = false;
            let exit = serve(
                stream,
                authed,
                &event_tx,
                &mut cmd_rx,
                &mut channels,
                &mut cursors,
                &mut delivered,
            )
            .await;

            // Only reset the backoff once the connection actually carried a
            // frame, so a socket that upgrades and immediately drops keeps
            // backing off instead of tight-looping (mirrors the raw client).
            if delivered {
                delays.reset();
            }

            match exit {
                LoopExit::Closed | LoopExit::ConsumerGone => return,
                // Transparent reconnect: nothing surfaced to the consumer.
                LoopExit::Reconnect => {}
            }
        }

        if backoff_wait(&mut delays, &mut cmd_rx, &mut channels, &mut cursors).await {
            return;
        }
    }
}

/// Resolve the connection URL, minting a fresh single-use `/ws/token` and
/// appending it as a query parameter when the stream needs authentication.
async fn connect_url(client: &Client, ws_url: &str, authed: bool) -> Result<String> {
    if !authed {
        return Ok(ws_url.to_string());
    }
    let token = client.mint_web_socket_token().await?;
    Ok(with_token(ws_url, &token.token))
}

/// Append `token` to `ws_url` as a properly-encoded `token=` query parameter,
/// preserving any existing query string. The token is single-use and
/// short-lived; it is never logged.
fn with_token(ws_url: &str, token: &str) -> String {
    // `serde_urlencoded` percent-encodes the value so an unusual token can't
    // alter the URL structure. Encoding a single string pair cannot fail.
    let pair = serde_urlencoded::to_string([("token", token)]).unwrap_or_default();
    let sep = if ws_url.contains('?') { '&' } else { '?' };
    format!("{ws_url}{sep}{pair}")
}

/// Drive a single live connection until it drops or the task is told to stop.
///
/// `authed` reflects whether this connection was opened with a token: a request
/// to subscribe to a *private* channel on an unauthenticated connection forces a
/// reconnect so the next attempt mints a token.
async fn serve<S>(
    stream: tokio_tungstenite::WebSocketStream<S>,
    authed: bool,
    event_tx: &mpsc::Sender<Item>,
    cmd_rx: &mut mpsc::Receiver<Command>,
    channels: &mut Vec<Channel>,
    cursors: &mut HashMap<CursorKey, u64>,
    delivered: &mut bool,
) -> LoopExit
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut write, mut read) = stream.split();

    // The reconnect helper in action: (re)subscribe every channel, carrying its
    // `since` cursor so the server resumes after the last frame we processed.
    for channel in channels.iter() {
        let since = cursors.get(&channel.cursor_key()).copied();
        if write
            .send(Message::Text(channel.subscribe_text(since).into()))
            .await
            .is_err()
        {
            return LoopExit::Reconnect;
        }
    }

    // Frames dropped while the consumer was behind; reported as `Error::Lagged`
    // before the next delivered frame.
    let mut dropped: u64 = 0;

    loop {
        tokio::select! {
            frame = read.next() => match frame {
                Some(Ok(msg)) => {
                    if let Some(exit) =
                        handle_frame(msg, event_tx, &mut write, cursors, delivered, &mut dropped).await
                    {
                        return exit;
                    }
                }
                Some(Err(_)) => return LoopExit::Reconnect,
                None => return LoopExit::Reconnect,
            },

            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Subscribe(channel)) => {
                    let key = channel.cursor_key();
                    if channels.iter().any(|c| c.cursor_key() == key) {
                        continue; // already subscribed
                    }
                    // Need a token to subscribe privately, but this socket has
                    // none: queue it and reconnect so the next attempt upgrades.
                    if channel.is_private() && !authed {
                        channels.push(channel);
                        return LoopExit::Reconnect;
                    }
                    let since = cursors.get(&key).copied();
                    let send = write
                        .send(Message::Text(channel.subscribe_text(since).into()))
                        .await;
                    // Record it regardless so a send failure still resubscribes
                    // after the reconnect that the failure triggers.
                    channels.push(channel);
                    if send.is_err() {
                        return LoopExit::Reconnect;
                    }
                }
                Some(Command::Unsubscribe(channel)) => {
                    let key = channel.cursor_key();
                    channels.retain(|c| c.cursor_key() != key);
                    cursors.remove(&key);
                    // Best-effort: a failed send means the socket is gone, which
                    // the read half will observe and reconnect on — and the
                    // channel is already out of the replay set.
                    let _ = write
                        .send(Message::Text(channel.unsubscribe_text().into()))
                        .await;
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
/// or `None` to keep going. Never blocks the read loop, so keepalive pings are
/// always read and ponged promptly.
async fn handle_frame<W>(
    msg: Message,
    event_tx: &mpsc::Sender<Item>,
    write: &mut W,
    cursors: &mut HashMap<CursorKey, u64>,
    delivered: &mut bool,
    dropped: &mut u64,
) -> Option<LoopExit>
where
    W: SinkExt<Message> + Unpin,
{
    match msg {
        Message::Text(text) => decode(
            text.as_str().as_bytes(),
            event_tx,
            cursors,
            delivered,
            dropped,
        ),
        Message::Binary(bytes) => decode(&bytes, event_tx, cursors, delivered, dropped),
        // Keepalive: pong promptly. A failed pong means the socket is gone.
        Message::Ping(payload) => match write.send(Message::Pong(payload)).await {
            Ok(()) => None,
            Err(_) => Some(LoopExit::Reconnect),
        },
        Message::Close(_) => Some(LoopExit::Reconnect),
        // Pong / raw Frame: nothing to do.
        _ => None,
    }
}

/// Decode an op-envelope payload, fold its cursor forward, and forward it to the
/// consumer. A frame that doesn't parse into a known [`ServerMessage`] (an
/// unknown future `op`, or a non-JSON text frame) is skipped rather than tearing
/// down an otherwise healthy connection.
fn decode(
    bytes: &[u8],
    event_tx: &mpsc::Sender<Item>,
    cursors: &mut HashMap<CursorKey, u64>,
    delivered: &mut bool,
    dropped: &mut u64,
) -> Option<LoopExit> {
    match serde_json::from_slice::<ServerMessage>(bytes) {
        Ok(msg) => {
            if let Some((key, seq)) = msg.cursor_advance() {
                let entry = cursors.entry(key).or_insert(0);
                // Take the max so a reordered or duplicate frame can't rewind a
                // cursor below a seq we've already processed.
                *entry = (*entry).max(seq);
            }
            // An `out_of_sync` invalidates the stale cursor: drop it so the next
            // (re)subscribe resumes from the live edge instead of replaying a
            // `since` the server can no longer satisfy. The frame is still
            // surfaced below so the consumer can REST-refetch.
            if let Some(key) = msg.cursor_reset() {
                cursors.remove(&key);
            }
            deliver(event_tx, msg, delivered, dropped)
        }
        Err(_) => None,
    }
}

/// Forward a decoded message without blocking the read loop.
///
/// On a full channel the frame is dropped and counted; the count is flushed as
/// an [`Error::Lagged`] immediately before the next successfully delivered
/// message, so the consumer sees gaps in order. Returns
/// [`LoopExit::ConsumerGone`] once the receiver is gone. Sets `delivered` on the
/// first hand-off, marking the connection healthy enough to reset the backoff.
fn deliver(
    event_tx: &mpsc::Sender<Item>,
    msg: ServerMessage,
    delivered: &mut bool,
    dropped: &mut u64,
) -> Option<LoopExit> {
    // Flush a pending lag marker first so the gap is reported in order, ahead of
    // the message that follows it.
    if *dropped > 0 {
        match event_tx.try_send(Err(Error::Lagged { dropped: *dropped })) {
            Ok(()) => *dropped = 0,
            Err(TrySendError::Full(_)) => {} // still no room; keep accumulating
            Err(TrySendError::Closed(_)) => return Some(LoopExit::ConsumerGone),
        }
    }

    match event_tx.try_send(Ok(msg)) {
        Ok(()) => {
            *delivered = true;
            None
        }
        Err(TrySendError::Full(_)) => {
            *dropped += 1;
            None
        }
        Err(TrySendError::Closed(_)) => Some(LoopExit::ConsumerGone),
    }
}

/// Try to deliver a one-off item (e.g. a surfaced error) without blocking.
/// Returns `false` only once the consumer is gone, so the caller can stop. A
/// full channel drops the item — the consumer is already behind and will see an
/// [`Error::Lagged`].
fn emit(event_tx: &mpsc::Sender<Item>, item: Item) -> bool {
    !matches!(event_tx.try_send(item), Err(TrySendError::Closed(_)))
}

/// Back off before the next reconnect. A `Close` (or a dropped handle) cuts the
/// wait short and signals the caller to stop (returns `true`). Subscribe /
/// unsubscribe commands are applied to the replay set but must **not** shorten
/// the backoff — otherwise a client churning its subscriptions while the
/// endpoint is down would defeat the backoff and hammer it — so a fixed deadline
/// is held across them.
async fn backoff_wait(
    delays: &mut crate::ws::BackoffIter,
    cmd_rx: &mut mpsc::Receiver<Command>,
    channels: &mut Vec<Channel>,
    cursors: &mut HashMap<CursorKey, u64>,
) -> bool {
    let deadline = tokio::time::Instant::now() + delays.next_delay();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Close) | None => return true,
                Some(Command::Subscribe(channel)) => {
                    let key = channel.cursor_key();
                    if !channels.iter().any(|c| c.cursor_key() == key) {
                        channels.push(channel);
                    }
                }
                Some(Command::Unsubscribe(channel)) => {
                    let key = channel.cursor_key();
                    channels.retain(|c| c.cursor_key() != key);
                    cursors.remove(&key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(seq: u64) -> ServerMessage {
        serde_json::from_value(json!({
            "op": "event", "channel": "trades", "market": "BTC-USDX-PERP",
            "seq": seq, "payload": { "n": seq }
        }))
        .unwrap()
    }

    /// `with_token` encodes the token and respects an existing query string.
    #[test]
    fn token_is_appended_and_encoded() {
        assert_eq!(
            with_token("wss://host/ws", "abc 123/x"),
            "wss://host/ws?token=abc+123%2Fx"
        );
        assert_eq!(
            with_token("wss://host/ws?v=1", "tok"),
            "wss://host/ws?v=1&token=tok"
        );
    }

    /// Drive `deliver` against a tiny channel, draining at controlled points so
    /// we exercise both the drop path (channel full) and the lag-report path,
    /// then prove the delivered stream is order-preserving and accounts for
    /// every frame exactly once (delivered, reported lagged, or still pending).
    #[test]
    fn deliver_drops_excess_and_reports_via_lagged() {
        const TOTAL: u64 = 7;
        let (tx, mut rx) = mpsc::channel::<Item>(2);
        let mut delivered = false;
        let mut dropped = 0u64;
        let mut items: Vec<Item> = Vec::new();

        for seq in 0..TOTAL {
            if seq == 4 || seq == 6 {
                while let Ok(item) = rx.try_recv() {
                    items.push(item);
                }
            }
            let exit = deliver(&tx, sample(seq), &mut delivered, &mut dropped);
            assert!(exit.is_none(), "consumer present, no exit expected");
        }
        drop(tx);
        while let Ok(item) = rx.try_recv() {
            items.push(item);
        }

        let mut expected = 0u64;
        let mut received = 0u64;
        let mut lagged_total = 0u64;
        for item in items {
            match item {
                Err(Error::Lagged { dropped }) => {
                    assert!(dropped > 0, "Lagged must report a positive gap");
                    expected += dropped;
                    lagged_total += dropped;
                }
                Ok(ServerMessage::Event { seq, .. }) => {
                    assert_eq!(seq, expected, "reorder/duplicate frame");
                    expected += 1;
                    received += 1;
                }
                other => panic!("unexpected item: {other:?}"),
            }
        }

        assert_eq!(
            received + lagged_total + dropped,
            TOTAL,
            "every frame is delivered, reported lagged, or still pending"
        );
        assert!(delivered, "first frame delivered before the channel fills");
        assert!(lagged_total > 0, "a Lagged marker should have flushed");
        assert!(received < TOTAL, "capacity 2 vs 7 frames must drop some");
    }

    /// `decode` folds cursors forward monotonically and ignores unparseable
    /// frames without tearing down the connection.
    #[test]
    fn decode_advances_cursor_and_skips_garbage() {
        let (tx, _rx) = mpsc::channel::<Item>(8);
        let mut cursors: HashMap<CursorKey, u64> = HashMap::new();
        let mut delivered = false;
        let mut dropped = 0u64;
        let key = Channel::trades("BTC-USDX-PERP").cursor_key();

        let event = json!({
            "op": "event", "channel": "trades", "market": "BTC-USDX-PERP",
            "seq": 5, "payload": {}
        })
        .to_string();
        decode(
            event.as_bytes(),
            &tx,
            &mut cursors,
            &mut delivered,
            &mut dropped,
        );
        assert_eq!(cursors.get(&key), Some(&5));

        // An older (reordered) seq must not rewind the cursor.
        let stale = json!({
            "op": "event", "channel": "trades", "market": "BTC-USDX-PERP",
            "seq": 3, "payload": {}
        })
        .to_string();
        decode(
            stale.as_bytes(),
            &tx,
            &mut cursors,
            &mut delivered,
            &mut dropped,
        );
        assert_eq!(cursors.get(&key), Some(&5));

        // An `out_of_sync` drops the stale cursor so the next subscribe omits
        // `since` and resumes from the live edge.
        let oos = json!({
            "op": "out_of_sync", "channel": "trades", "market": "BTC-USDX-PERP", "oldest_seq": 9
        })
        .to_string();
        decode(
            oos.as_bytes(),
            &tx,
            &mut cursors,
            &mut delivered,
            &mut dropped,
        );
        assert_eq!(cursors.get(&key), None, "out_of_sync clears the cursor");

        // Non-JSON / unknown frames are skipped, leaving cursors untouched.
        let before = cursors.clone();
        assert!(decode(b"not json", &tx, &mut cursors, &mut delivered, &mut dropped).is_none());
        assert!(decode(
            br#"{"op":"future_op"}"#,
            &tx,
            &mut cursors,
            &mut delivered,
            &mut dropped
        )
        .is_none());
        assert_eq!(cursors, before);
    }
}
