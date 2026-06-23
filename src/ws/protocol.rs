//! The op-envelope wire protocol for the streaming API.
//!
//! Every frame on the socket — in both directions — is a JSON object carrying
//! an `op` discriminator (an "op envelope"). Outbound the client sends
//! `subscribe` / `unsubscribe` ops naming a [`Channel`]; inbound the server
//! sends `subscribed` / `unsubscribed` acknowledgements, `event` data frames,
//! `out_of_sync` gap signals, and `error` frames, all decoded into
//! [`ServerMessage`].
//!
//! These types **mirror the wire protocol verbatim**. In particular, `book`
//! updates are forwarded exactly as the server sends them (snapshots and
//! deltas); this layer does **not** reconstruct a local order book.
//!
//! # Cursors
//!
//! Each per-stream `event` carries a monotonically increasing `seq`, and each
//! `subscribed` acknowledgement carries the `seq_at_join` the stream was at when
//! the subscription took effect. The streaming client records the highest `seq`
//! seen per channel and, on reconnect, replays each `subscribe` with a `since`
//! cursor so the server resumes *after* the last frame the client processed —
//! closing the gap that an unqualified resubscribe would leave. If the client's
//! cursor predates the server's ring buffer the server sends `out_of_sync`
//! instead; the client drops that channel's cursor (so it stops asking for a
//! `since` the server can't satisfy) and surfaces the frame so the consumer can
//! REST-refetch. See [`crate::ws::MessageStream`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Identity of a subscribed stream, used to track resume cursors and to
/// de-duplicate the replay set. A `(channel, market, interval)` triple: the
/// extra dimensions are `None` for channels that don't use them (e.g. `orders`
/// has no market; `trades` has no interval), so two distinct streams never
/// collide on one cursor.
pub(crate) type CursorKey = (&'static str, Option<String>, Option<String>);

/// A channel to subscribe to over the streaming API — the op-envelope
/// subscription selector.
///
/// Public channels are **per-market** and need no authentication. Account
/// channels stream the authenticated account's private activity and require a
/// minted `/ws/token` (see [`Client::subscribe`](crate::Client::subscribe)),
/// which the client presents when upgrading the connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Channel {
    /// Public trade prints for a market, e.g. `BTC-USDX-PERP`.
    Trades {
        /// Market identifier.
        market: String,
    },
    /// Public order-book updates for a market. Frames are forwarded verbatim
    /// (snapshots and deltas as the server sends them); this client does not
    /// reconstruct the book.
    Book {
        /// Market identifier.
        market: String,
    },
    /// Public OHLCV candles for a market at a given interval (e.g. `1m`).
    Candles {
        /// Market identifier.
        market: String,
        /// Candle interval, e.g. `1m`, `5m`, `1h`.
        interval: String,
    },
    /// The authenticated account's order lifecycle updates. Private.
    Orders,
    /// The authenticated account's fills (private trade executions). Private.
    Fills,
    /// The authenticated account's position updates. Private.
    Positions,
    /// The authenticated account's balance updates. Private.
    Balances,
}

impl Channel {
    /// Subscribe to public trades for `market`.
    pub fn trades(market: impl Into<String>) -> Self {
        Self::Trades {
            market: market.into(),
        }
    }

    /// Subscribe to the public order book for `market`.
    pub fn book(market: impl Into<String>) -> Self {
        Self::Book {
            market: market.into(),
        }
    }

    /// Subscribe to public candles for `market` at `interval` (e.g. `1m`).
    pub fn candles(market: impl Into<String>, interval: impl Into<String>) -> Self {
        Self::Candles {
            market: market.into(),
            interval: interval.into(),
        }
    }

    /// Whether this channel streams private, account-scoped data and therefore
    /// requires an authenticated (token-upgraded) connection.
    pub fn is_private(&self) -> bool {
        matches!(
            self,
            Channel::Orders | Channel::Fills | Channel::Positions | Channel::Balances
        )
    }

    /// The wire name of this channel (the `channel` field of every frame).
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Channel::Trades { .. } => "trades",
            Channel::Book { .. } => "book",
            Channel::Candles { .. } => "candles",
            Channel::Orders => "orders",
            Channel::Fills => "fills",
            Channel::Positions => "positions",
            Channel::Balances => "balances",
        }
    }

    fn market(&self) -> Option<&str> {
        match self {
            Channel::Trades { market }
            | Channel::Book { market }
            | Channel::Candles { market, .. } => Some(market),
            Channel::Orders | Channel::Fills | Channel::Positions | Channel::Balances => None,
        }
    }

    fn interval(&self) -> Option<&str> {
        match self {
            Channel::Candles { interval, .. } => Some(interval),
            _ => None,
        }
    }

    /// Stable identity for cursor tracking and replay-set de-duplication.
    pub(crate) fn cursor_key(&self) -> CursorKey {
        (
            self.name(),
            self.market().map(str::to_string),
            self.interval().map(str::to_string),
        )
    }

    /// Serialize the `subscribe` op for this channel, carrying the `since`
    /// resume cursor when one is known.
    pub(crate) fn subscribe_text(&self, since: Option<u64>) -> String {
        let frame = OutboundFrame {
            op: "subscribe",
            channel: self.name(),
            market: self.market(),
            interval: self.interval(),
            since,
        };
        // Serializing a fixed, well-typed struct cannot fail; fall back to a
        // hand-built frame rather than panicking on the impossible.
        serde_json::to_string(&frame)
            .unwrap_or_else(|_| format!(r#"{{"op":"subscribe","channel":"{}"}}"#, self.name()))
    }

    /// Serialize the `unsubscribe` op for this channel.
    pub(crate) fn unsubscribe_text(&self) -> String {
        let frame = OutboundFrame {
            op: "unsubscribe",
            channel: self.name(),
            market: self.market(),
            interval: self.interval(),
            since: None,
        };
        serde_json::to_string(&frame)
            .unwrap_or_else(|_| format!(r#"{{"op":"unsubscribe","channel":"{}"}}"#, self.name()))
    }
}

/// An outbound op-envelope frame. Optional fields are omitted when absent so the
/// wire form is minimal and matches what the server expects per channel.
#[derive(Serialize)]
struct OutboundFrame<'a> {
    op: &'static str,
    channel: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    market: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<u64>,
}

/// Engine-stamped metadata on an `event` frame (D-044): the matching engine's
/// `epoch` (bumped on engine restart / failover), the monotonic `sequence`
/// within that epoch, and the `emitted_at` time the engine produced the frame.
/// This is the cross-process gap signal — a jump in `(epoch, sequence)` tells a
/// consumer the engine restarted or it missed engine output, independent of the
/// per-stream `seq` resume cursor. Absent on frames the engine doesn't stamp.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct EngineEnvelope {
    /// Engine epoch; increments when the matching engine restarts or fails over.
    #[serde(default)]
    pub epoch: u64,
    /// Monotonic sequence the engine assigned within `epoch`.
    #[serde(default)]
    pub sequence: u64,
    /// Unix ms when the engine emitted the frame.
    #[serde(default)]
    pub emitted_at: i64,
}

/// A decoded inbound op-envelope frame from the server.
///
/// This mirrors the wire protocol: payloads ([`Event::payload`](Self::Event))
/// are surfaced as raw [`serde_json::Value`] exactly as received, with no
/// transformation or order-book reconstruction. Unknown frames (a future `op`
/// the SDK doesn't model) are skipped by the streaming client rather than
/// decoded into a variant here, so this enum is `#[non_exhaustive]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    /// Acknowledges that a subscription is active. `seq_at_join` is the stream's
    /// sequence number at the moment the subscription took effect — the baseline
    /// the client resumes from if the connection drops before any `update`.
    Subscribed {
        /// Channel that was subscribed.
        channel: String,
        /// Market the subscription is for, if the channel is per-market.
        #[serde(default)]
        market: Option<String>,
        /// Candle interval, if applicable.
        #[serde(default)]
        interval: Option<String>,
        /// Stream sequence number at the moment of joining.
        seq_at_join: u64,
    },
    /// Acknowledges that a subscription was removed.
    Unsubscribed {
        /// Channel that was unsubscribed.
        channel: String,
        /// Market the subscription was for, if per-market.
        #[serde(default)]
        market: Option<String>,
        /// Candle interval, if applicable.
        #[serde(default)]
        interval: Option<String>,
    },
    /// A data frame for a subscribed channel (`op: "event"`), carrying a
    /// monotonic `seq` and the raw payload exactly as the server sent it.
    Event {
        /// Channel the event belongs to.
        channel: String,
        /// Market the event is for, if the channel is per-market.
        #[serde(default)]
        market: Option<String>,
        /// Candle interval, if the server echoes one (absent on most frames).
        #[serde(default)]
        interval: Option<String>,
        /// Monotonically increasing per-stream sequence number.
        seq: u64,
        /// Engine gap-signal metadata, when the engine stamped the frame.
        #[serde(default)]
        engine_envelope: Option<EngineEnvelope>,
        /// The payload, forwarded verbatim (no client-side reconstruction).
        payload: Value,
    },
    /// The client's resume cursor predates the server's ring buffer, so the
    /// requested `since` can no longer be satisfied: there is a real gap. The
    /// consumer must REST-refetch the current state and treat the stream as
    /// resumed from now. Non-fatal — the connection stays up, and the client
    /// drops this channel's cursor so it stops requesting an unsatisfiable
    /// `since`.
    OutOfSync {
        /// Channel that overran its buffer.
        channel: String,
        /// Market the channel is for, if per-market.
        #[serde(default)]
        market: Option<String>,
        /// Oldest sequence the server can still serve for this stream, if known.
        #[serde(default)]
        oldest_seq: Option<u64>,
    },
    /// A protocol-level error reported by the server (e.g. a bad subscription).
    /// This is a normal, non-fatal frame — the connection stays up.
    Error {
        /// Human-readable message, if any.
        #[serde(default)]
        message: Option<String>,
    },
}

impl ServerMessage {
    /// The resume cursor this frame advances, if any: the `(key, seq)` to fold
    /// into the cursor map. `event` frames advance to their `seq`; `subscribed`
    /// acknowledgements seed the baseline from `seq_at_join`. Other frames carry
    /// no cursor.
    pub(crate) fn cursor_advance(&self) -> Option<(CursorKey, u64)> {
        match self {
            ServerMessage::Event {
                channel,
                market,
                interval,
                seq,
                ..
            } => Some((key_of(channel, market, interval), *seq)),
            ServerMessage::Subscribed {
                channel,
                market,
                interval,
                seq_at_join,
            } => Some((key_of(channel, market, interval), *seq_at_join)),
            _ => None,
        }
    }

    /// The resume cursor this frame *invalidates*, if any. An `out_of_sync`
    /// frame means the server can no longer satisfy this stream's `since`, so
    /// the client drops that cursor — the next (re)subscribe omits `since` and
    /// resumes from the live edge instead of replaying an unsatisfiable gap.
    pub(crate) fn cursor_reset(&self) -> Option<CursorKey> {
        match self {
            ServerMessage::OutOfSync {
                channel, market, ..
            } => Some(key_of(channel, market, &None)),
            _ => None,
        }
    }
}

/// Build a [`CursorKey`] from the wire fields of an inbound frame, normalizing
/// the channel name to the same `&'static str` a [`Channel`] produces so inbound
/// and outbound keys match.
fn key_of(channel: &str, market: &Option<String>, interval: &Option<String>) -> CursorKey {
    let name = match channel {
        "trades" => "trades",
        "book" => "book",
        "candles" => "candles",
        "orders" => "orders",
        "fills" => "fills",
        "positions" => "positions",
        "balances" => "balances",
        // An unknown channel can't collide with a known cursor key; keep it
        // stable so repeated frames for it still fold consistently.
        _ => "",
    };
    (name, market.clone(), interval.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subscribe_frame_includes_only_relevant_fields() {
        let trades: Value =
            serde_json::from_str(&Channel::trades("BTC-USDX-PERP").subscribe_text(None)).unwrap();
        assert_eq!(
            trades,
            json!({ "op": "subscribe", "channel": "trades", "market": "BTC-USDX-PERP" })
        );

        // Candles carry an interval; a resume cursor adds `since`.
        let candles: Value =
            serde_json::from_str(&Channel::candles("ETH-USDX-PERP", "1m").subscribe_text(Some(42)))
                .unwrap();
        assert_eq!(
            candles,
            json!({
                "op": "subscribe",
                "channel": "candles",
                "market": "ETH-USDX-PERP",
                "interval": "1m",
                "since": 42
            })
        );

        // Account channels have neither market nor interval.
        let orders: Value = serde_json::from_str(&Channel::Orders.subscribe_text(None)).unwrap();
        assert_eq!(orders, json!({ "op": "subscribe", "channel": "orders" }));
    }

    #[test]
    fn private_channels_are_classified() {
        assert!(Channel::Orders.is_private());
        assert!(Channel::Positions.is_private());
        assert!(!Channel::trades("BTC-USDX-PERP").is_private());
        assert!(!Channel::candles("BTC-USDX-PERP", "1m").is_private());
    }

    #[test]
    fn decodes_each_frame_kind_and_extracts_cursor() {
        let subscribed: ServerMessage = serde_json::from_value(json!({
            "op": "subscribed", "channel": "trades", "market": "BTC-USDX-PERP", "seq_at_join": 100
        }))
        .unwrap();
        // `subscribed` seeds the resume baseline from `seq_at_join`.
        let (key, seq) = subscribed.cursor_advance().unwrap();
        assert_eq!(key, ("trades", Some("BTC-USDX-PERP".to_string()), None));
        assert_eq!(seq, 100);

        let event: ServerMessage = serde_json::from_value(json!({
            "op": "event", "channel": "trades", "market": "BTC-USDX-PERP", "seq": 105,
            "payload": { "price": "42000", "size": "0.1" }
        }))
        .unwrap();
        let (key, seq) = event.cursor_advance().unwrap();
        assert_eq!(key, ("trades", Some("BTC-USDX-PERP".to_string()), None));
        assert_eq!(seq, 105);

        // An `event` may carry engine-stamped gap metadata.
        let stamped: ServerMessage = serde_json::from_value(json!({
            "op": "event", "channel": "trades", "market": "BTC-USDX-PERP", "seq": 106,
            "engine_envelope": { "epoch": 3, "sequence": 9001, "emitted_at": 1_776_000_000_000i64 },
            "payload": {}
        }))
        .unwrap();
        match stamped {
            ServerMessage::Event {
                engine_envelope: Some(env),
                ..
            } => {
                assert_eq!(env.epoch, 3);
                assert_eq!(env.sequence, 9001);
            }
            other => panic!("expected stamped event, got {other:?}"),
        }

        // `out_of_sync` carries no advance cursor but resets the stream's cursor.
        let oos: ServerMessage = serde_json::from_value(json!({
            "op": "out_of_sync", "channel": "trades", "market": "BTC-USDX-PERP", "oldest_seq": 200
        }))
        .unwrap();
        assert!(oos.cursor_advance().is_none());
        assert_eq!(
            oos.cursor_reset().unwrap(),
            ("trades", Some("BTC-USDX-PERP".to_string()), None)
        );

        // Server error frames carry only `message`.
        let err: ServerMessage = serde_json::from_value(json!({
            "op": "error", "message": "no such market"
        }))
        .unwrap();
        assert!(matches!(err, ServerMessage::Error { .. }));
        assert!(err.cursor_advance().is_none());
    }

    #[test]
    fn inbound_and_outbound_cursor_keys_match() {
        // The key derived from a Channel must equal the key derived from the
        // server's frame for the same stream, or resume cursors never line up.
        let channel = Channel::candles("BTC-USDX-PERP", "1m");
        let msg: ServerMessage = serde_json::from_value(json!({
            "op": "event", "channel": "candles", "market": "BTC-USDX-PERP",
            "interval": "1m", "seq": 7, "payload": {}
        }))
        .unwrap();
        assert_eq!(channel.cursor_key(), msg.cursor_advance().unwrap().0);
    }
}
