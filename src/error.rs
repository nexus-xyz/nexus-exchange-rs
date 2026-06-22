//! Error types.

use std::time::Duration;

use crate::markets::OrderError;
use thiserror::Error;

/// Errors returned by the SDK.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Transport or HTTP-layer failure.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// Failed to (de)serialize a request or response body.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// The API returned a structured error envelope (`{ code, message }`).
    #[error("api error [{code}] (http {status}): {message}")]
    Api {
        /// HTTP status code the API responded with.
        status: u16,
        /// Machine-readable error code.
        code: String,
        /// Human-readable message.
        message: String,
    },

    /// A WebSocket transport or protocol failure.
    #[error("websocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),

    /// The streaming client's background task has stopped, so commands such as
    /// [`subscribe`](crate::ws::Subscription::subscribe) can no longer be sent.
    #[error("websocket stream is closed")]
    StreamClosed,

    /// The consumer of a typed [`MessageStream`](crate::ws::MessageStream) fell
    /// behind and `dropped` message frames were discarded to keep the socket
    /// drained (so keepalive pongs are never starved). Surfaced in order,
    /// immediately before the next delivered message; the stream continues —
    /// this is a gap signal, not a fatal error.
    #[error("websocket consumer lagged; {dropped} message(s) dropped")]
    Lagged {
        /// Number of message frames dropped since the last delivered message.
        dropped: u64,
    },

    /// Authentication problem (missing credentials, malformed secret, etc.).
    #[error("authentication error: {0}")]
    Auth(String),

    /// An order failed local validation against a market's trading rules
    /// before submission. See [`OrderError`].
    #[error("invalid order: {0}")]
    InvalidOrder(#[from] OrderError),

    /// A request failed local validation before being sent — e.g. an amend
    /// with no changes, non-positive leverage or transfer amount, an empty
    /// batch, or an empty identifier. The request is never transmitted.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The API returned `429 Too Many Requests` and automatic retries were
    /// exhausted. `retry_after` carries the server's `Retry-After` hint, if any.
    #[error("rate limited (retries exhausted){}", match .retry_after {
        Some(d) => format!("; retry after {}s", d.as_secs()),
        None => String::new(),
    })]
    RateLimited {
        /// How long the server asked the caller to wait before retrying.
        retry_after: Option<Duration>,
    },
}

impl Error {
    /// Whether this error is *transient* — a failure that may succeed if the
    /// request is simply retried, as opposed to a deterministic failure (bad
    /// input, a 4xx the server will keep rejecting, a malformed body) that
    /// would fail identically every time.
    ///
    /// This is the predicate the retry layer uses to decide what to retry; see
    /// [`Config::with_retry`](crate::Config::with_retry). The transient classes are:
    ///
    /// - **Connect/timeout transport errors** — the connection never completed,
    ///   so the request likely never reached the server. Note only
    ///   [`reqwest::Error::is_timeout`]/[`is_connect`](reqwest::Error::is_connect)
    ///   count: an error mid-body-read (e.g. a decode failure) is *not*
    ///   transient, since replaying it would fail the same way.
    /// - **HTTP 408 (Request Timeout)** — the server timed out waiting for the
    ///   request and invites a retry.
    /// - **HTTP 5xx** — a server-side fault that is often momentary.
    ///
    /// Everything else — other 4xx, deserialization failures, body-decode
    /// errors — is treated as terminal and is *not* retried.
    ///
    /// **`429` is deliberately excluded here.** Rate limiting is owned
    /// end-to-end by the dedicated rate-limit layer ([`RateLimit`](crate::RateLimit)),
    /// which honors `Retry-After` and surfaces exhaustion as
    /// [`Error::RateLimited`]. Classifying `429` as transient here too would
    /// double-retry it — once with backoff that ignores `Retry-After`, once with
    /// it — so this generic layer stays out of the way and leaves `429` to the
    /// single owner.
    pub fn is_transient(&self) -> bool {
        match self {
            // Only connect/timeout failures are retried; a body-read error that
            // sets neither flag is deterministic and treated as terminal.
            Error::Http(e) => e.is_timeout() || e.is_connect(),
            Error::Api { status, .. } => *status == 408 || (500..600).contains(status),
            // Everything else — serde/body-decode, auth, invalid order/request,
            // websocket, and already-exhausted rate limiting — is terminal for
            // this generic retry layer.
            _ => false,
        }
    }
}
