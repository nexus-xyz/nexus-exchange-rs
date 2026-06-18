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
    /// - **HTTP 408 (Request Timeout)** — the server timed out waiting for the
    ///   request and invites a retry.
    /// - **HTTP 5xx** — a server-side fault that is often momentary.
    ///
    /// Everything else — other 4xx, deserialization failures, body-decode
    /// errors — is treated as terminal and is *not* retried.
    ///
    /// **`429` is deliberately excluded here.** Rate limiting is owned end-to-end
    /// by the rate-limit layer (`GET /account/rate-limit` + the cost-weighted
    /// token bucket), which retries `429` honoring `Retry-After`/`X-RateLimit-*`
    /// and otherwise surfaces a terminal `Error::RateLimited`. Classifying `429`
    /// as transient here too would retry it twice — once with backoff that
    /// ignores `Retry-After`, once with it — so this generic layer stays out of
    /// the way and lets the rate-limit layer be the single owner.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http(e) => e.is_timeout() || e.is_connect(),
            Error::Api { status, .. } => *status == 408 || (500..600).contains(status),
            Error::Serde(_) => false,
        }
    }
}
