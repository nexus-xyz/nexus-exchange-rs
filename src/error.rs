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
    #[error("api error [{code}]: {message}")]
    Api {
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
