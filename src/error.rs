//! Error types.

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
    ///   so the request likely never reached the server.
    /// - **HTTP 408 (Request Timeout)** and **429 (Too Many Requests)** — the
    ///   server is explicitly asking us to slow down and try again.
    /// - **HTTP 5xx** — a server-side fault that is often momentary.
    ///
    /// Everything else — other 4xx, deserialization failures, body-decode
    /// errors — is treated as terminal and is *not* retried.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http(e) => e.is_timeout() || e.is_connect(),
            Error::Api { status, .. } => {
                *status == 408 || *status == 429 || (500..600).contains(status)
            }
            Error::Serde(_) => false,
        }
    }
}
