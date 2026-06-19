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
    /// **`429` is deliberately excluded here.** Today a `429` surfaces as a
    /// terminal `Error::Api { status: 429, .. }`. Rate limiting is intended to be
    /// owned end-to-end by the dedicated rate-limit layer (tracked separately;
    /// land after that PR), which will honor `Retry-After`/`X-RateLimit-*`.
    /// Classifying `429` as transient here too would double-retry it — once with
    /// backoff that ignores `Retry-After`, once with it — so this generic layer
    /// stays out of the way and leaves `429` to the single owner.
    pub fn is_transient(&self) -> bool {
        match self {
            // Only connect/timeout failures are retried; a body-read error that
            // sets neither flag is deterministic and treated as terminal.
            Error::Http(e) => e.is_timeout() || e.is_connect(),
            Error::Api { status, .. } => *status == 408 || (500..600).contains(status),
            Error::Serde(_) => false,
        }
    }
}
