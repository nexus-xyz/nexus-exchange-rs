//! Error types.
//!
//! Errors are split into two trees so callers can branch on whether a retry
//! could help — mirroring ccxt's `ExchangeError` (terminal) vs `NetworkError`
//! (transient), and deliberately avoiding the anyhow-collapse where every
//! failure is an opaque string.
//!
//! - [`Error::is_retryable`] — drives retry/backoff (the retry layer retries
//!   only [`Error::Transient`]).
//! - [`Error::retry_after`] — server-advised delay for `429` handling.
//! - [`Error::exit_code`] — process exit status for the CLI.

use std::time::Duration;

use crate::markets::OrderError;
use thiserror::Error;

/// Top-level SDK error: a terminal failure or a transient one.
///
/// `#[non_exhaustive]` so a future third category (e.g. a partial/degraded
/// class) can be added without a major version bump. Callers should match
/// `Terminal` / `Transient` with a catch-all `_` arm; the sub-enums are
/// `#[non_exhaustive]` for the same reason.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The request will not succeed if retried as-is (auth, bad request,
    /// business rejections, local validation). Mirrors ccxt `ExchangeError` —
    /// surface to the caller.
    #[error(transparent)]
    Terminal(#[from] TerminalError),

    /// A retry — with backoff, honoring [`Error::retry_after`] — may succeed.
    /// Mirrors ccxt `NetworkError`.
    #[error(transparent)]
    Transient(#[from] TransientError),
}

/// Terminal failures — retrying as-is won't help.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TerminalError {
    /// The account lacks the balance/margin for the operation
    /// (gateway `INSUFFICIENT_BALANCE`, engine `InsufficientMargin`).
    #[error("insufficient funds: {message}")]
    InsufficientFunds {
        /// Human-readable detail from the server.
        message: String,
    },

    /// The order was rejected by the server on its own terms — tick/lot size,
    /// leverage, amend, size (engine `InvalidTickSize` / `InvalidLeverage` /
    /// …). For an order rejected by *local* validation before submission, see
    /// [`TerminalError::OrderValidation`].
    #[error("invalid order [{code}]: {message}")]
    InvalidOrder {
        /// Machine-readable code from the server.
        code: String,
        /// Human-readable detail from the server.
        message: String,
    },

    /// An order failed local validation against a market's trading rules
    /// before submission — the request is never transmitted. See [`OrderError`].
    #[error("invalid order: {0}")]
    OrderValidation(#[from] OrderError),

    /// A request failed local validation before being sent — e.g. an amend
    /// with no changes, non-positive leverage or transfer amount, an empty
    /// batch, or an empty identifier. The request is never transmitted.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Authentication or authorization failed on the server — missing/expired
    /// key, bad signature, or unrecognized agent/wallet (HTTP 401/403, or
    /// `UNAUTHORIZED` / `FORBIDDEN` / `SIGNATURE_INVALID` / `BAD_AGENT` /
    /// `BAD_WALLET`).
    #[error("authentication failed [{code}]: {message}")]
    Auth {
        /// Machine-readable code from the server.
        code: String,
        /// Human-readable detail from the server.
        message: String,
    },

    /// A local credential problem — missing credentials for a signed endpoint,
    /// a malformed secret/private key, or a signing failure. Detected before
    /// any request is sent, so a retry would fail identically.
    #[error("credential error: {0}")]
    Credentials(String),

    /// A malformed or otherwise-rejected request the SDK doesn't model
    /// specifically. Carries the server's machine-readable code so callers can
    /// still match on it.
    #[error("bad request [{code}]: {message}")]
    BadRequest {
        /// Machine-readable code from the server.
        code: String,
        /// Human-readable detail from the server.
        message: String,
    },

    /// A response body could not be decoded into the expected type (or a
    /// request body could not be serialized) — a client/contract mismatch.
    /// Not retryable: the same bytes will fail again.
    #[error("response decode failed: {0}")]
    Decode(#[source] serde_json::Error),
}

/// Transient failures — a retry (with backoff) may succeed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransientError {
    /// Rate limited (HTTP 429). Honor `retry_after` when the server sets a
    /// `Retry-After` header.
    #[error("rate limited")]
    RateLimited {
        /// Server-advised delay from `Retry-After`, if present.
        retry_after: Option<Duration>,
    },

    /// No response within the configured deadline.
    #[error("request timed out")]
    Timeout,

    /// The gateway is reachable but not serving — HTTP 502/503/504 or 5xx
    /// (deploy, overload, storage blip).
    #[error("service unavailable [{status}]: {message}")]
    Unavailable {
        /// HTTP status code.
        status: u16,
        /// Human-readable detail from the server (may be empty).
        message: String,
    },

    /// Transport-layer failure before any HTTP status — connect, DNS, TLS,
    /// connection reset. (reqwest errors, minus timeouts which map to
    /// [`TransientError::Timeout`].)
    #[error("network error: {0}")]
    Network(#[source] reqwest::Error),

    /// A WebSocket transport or protocol failure. The streaming client
    /// reconnects with backoff on these internally; when one does surface, a
    /// reconnect may recover it.
    #[error("websocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),

    /// The streaming client's background task has stopped, so commands such as
    /// [`subscribe`](crate::ws::Subscription::subscribe) can no longer be sent.
    /// Recoverable by establishing a fresh streaming client.
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
}

// reqwest errors are transport-level — classify, never collapse.
impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            TransientError::Timeout.into()
        } else {
            TransientError::Network(e).into()
        }
    }
}

// A (de)serialization failure is a client/contract mismatch — terminal.
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        TerminalError::Decode(e).into()
    }
}

// A local order-validation failure is terminal — the request never leaves.
impl From<OrderError> for Error {
    fn from(e: OrderError) -> Self {
        TerminalError::OrderValidation(e).into()
    }
}

// A WebSocket transport/protocol failure is transient — a reconnect may help.
impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        TransientError::Ws(e).into()
    }
}

impl Error {
    /// A local credential problem (missing credentials, malformed secret,
    /// signing failure). Shorthand for [`TerminalError::Credentials`].
    pub(crate) fn credentials(message: impl Into<String>) -> Self {
        TerminalError::Credentials(message.into()).into()
    }

    /// A request that failed local pre-flight validation and was never sent.
    /// Shorthand for [`TerminalError::InvalidRequest`].
    pub(crate) fn invalid_request(message: impl Into<String>) -> Self {
        TerminalError::InvalidRequest(message.into()).into()
    }

    /// The streaming client's background task has stopped. Shorthand for
    /// [`TransientError::StreamClosed`].
    pub(crate) fn stream_closed() -> Self {
        TransientError::StreamClosed.into()
    }

    /// The typed-stream consumer fell behind and `dropped` frames were
    /// discarded. Shorthand for [`TransientError::Lagged`].
    pub(crate) fn lagged(dropped: u64) -> Self {
        TransientError::Lagged { dropped }.into()
    }

    /// Classify a non-2xx HTTP response into the appropriate tree.
    ///
    /// `code`/`message` come from the API's `{ code, message }` envelope when
    /// present; the caller passes the HTTP status as a fallback code/message
    /// otherwise. The transient/terminal split is driven by **status** (the
    /// reliable signal — safe retries don't depend on the body parsing); the
    /// specific 4xx variant is then refined by the machine-readable `code`.
    pub(crate) fn from_api(
        status: reqwest::StatusCode,
        retry_after: Option<Duration>,
        code: String,
        message: String,
    ) -> Self {
        use reqwest::StatusCode as S;
        match status {
            S::TOO_MANY_REQUESTS => return TransientError::RateLimited { retry_after }.into(),
            // A server-side request timeout is worth retrying (distinct from a
            // client-side timeout, which maps to `Timeout` via the reqwest
            // `From` impl).
            S::REQUEST_TIMEOUT => return TransientError::Timeout.into(),
            S::UNAUTHORIZED | S::FORBIDDEN => {
                return TerminalError::Auth { code, message }.into();
            }
            // `501 Not Implemented` / `505 HTTP Version Not Supported` are
            // permanent — retrying can't change the outcome — so fall through
            // to the terminal mapping below rather than treating them as a
            // transient `Unavailable`.
            S::NOT_IMPLEMENTED | S::HTTP_VERSION_NOT_SUPPORTED => {}
            _ if status.is_server_error() => {
                // Other 5xx (500/502/503/504, …) — server-side, a retry may
                // succeed.
                return TransientError::Unavailable {
                    status: status.as_u16(),
                    message,
                }
                .into();
            }
            _ => {}
        }

        // Remaining 4xx — map the machine-readable code to a terminal variant.
        // Codes come from the gateway (indexer, SCREAMING_SNAKE) and the engine
        // (PascalCase `Invalid*`). Unknown codes fall back to `BadRequest` —
        // still terminal, correct tree. Refine the partition as the API's error
        // catalog stabilizes.
        if is_insufficient_funds_code(&code) {
            TerminalError::InsufficientFunds { message }.into()
        } else if is_auth_code(&code) {
            TerminalError::Auth { code, message }.into()
        } else if is_order_reject_code(&code) {
            TerminalError::InvalidOrder { code, message }.into()
        } else {
            TerminalError::BadRequest { code, message }.into()
        }
    }

    /// Whether retrying the request (with backoff) could succeed. True for
    /// every [`Error::Transient`]; false for terminal failures.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Error::Transient(_))
    }

    /// Server-advised delay before retrying, set only for a rate-limited
    /// response that carried a `Retry-After` header.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::Transient(TransientError::RateLimited { retry_after }) => *retry_after,
            _ => None,
        }
    }

    /// Process exit code for the CLI, following the `sysexits.h` convention:
    /// `75` EX_TEMPFAIL (transient), `77` EX_NOPERM (auth/credentials), `65`
    /// EX_DATAERR (other terminal).
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Transient(_) => 75,
            Error::Terminal(TerminalError::Auth { .. })
            | Error::Terminal(TerminalError::Credentials(_)) => 77,
            Error::Terminal(_) => 65,
        }
    }
}

/// Insufficient-balance/margin codes → [`TerminalError::InsufficientFunds`].
/// Matched exactly (gateway SCREAMING_SNAKE `INSUFFICIENT_BALANCE`, engine
/// PascalCase `InsufficientMargin`) rather than by substring, so unrelated
/// codes like `INSUFFICIENT_PERMISSIONS` don't surface a misleading
/// "insufficient funds" message.
fn is_insufficient_funds_code(code: &str) -> bool {
    matches!(code, "INSUFFICIENT_BALANCE" | "InsufficientMargin")
}

/// Auth/authorization codes that map to [`TerminalError::Auth`] even on a
/// generic 4xx (the 401/403 status path catches most, but the gateway returns
/// some of these with a 400).
fn is_auth_code(code: &str) -> bool {
    matches!(
        code,
        "UNAUTHORIZED" | "FORBIDDEN" | "SIGNATURE_INVALID" | "BAD_AGENT" | "BAD_WALLET"
    )
}

/// Order-parameter rejection codes (engine, PascalCase) →
/// [`TerminalError::InvalidOrder`]. Request-shape codes (`InvalidBody`,
/// `InvalidQuery`, `InvalidMarket`, …) stay [`TerminalError::BadRequest`].
fn is_order_reject_code(code: &str) -> bool {
    matches!(
        code,
        "InvalidTickSize" | "InvalidLotSize" | "InvalidLeverage" | "InvalidAmend" | "InvalidAmount"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    fn classify(status: u16, code: &str) -> Error {
        Error::from_api(
            StatusCode::from_u16(status).unwrap(),
            None,
            code.to_string(),
            "detail".to_string(),
        )
    }

    #[test]
    fn rate_limited_is_transient_and_carries_retry_after() {
        let e = Error::from_api(
            StatusCode::TOO_MANY_REQUESTS,
            Some(Duration::from_secs(3)),
            "RATE_LIMITED".into(),
            "slow down".into(),
        );
        assert!(e.is_retryable());
        assert_eq!(e.retry_after(), Some(Duration::from_secs(3)));
        assert!(matches!(
            e,
            Error::Transient(TransientError::RateLimited { .. })
        ));
    }

    #[test]
    fn five_xx_is_transient_unavailable() {
        for s in [500u16, 502, 503, 504] {
            let e = classify(s, "STORAGE_ERROR");
            assert!(e.is_retryable(), "status {s} should be retryable");
            assert!(matches!(
                e,
                Error::Transient(TransientError::Unavailable { .. })
            ));
        }
    }

    #[test]
    fn request_timeout_is_transient() {
        let e = classify(408, "REQUEST_TIMEOUT");
        assert!(e.is_retryable(), "408 should be retryable");
        assert!(matches!(e, Error::Transient(TransientError::Timeout)));
    }

    #[test]
    fn permanent_5xx_is_terminal() {
        // 501 Not Implemented / 505 HTTP Version Not Supported won't change on
        // retry — terminal, not a transient `Unavailable`.
        for s in [501u16, 505] {
            let e = classify(s, "NOT_IMPLEMENTED");
            assert!(!e.is_retryable(), "status {s} should not be retryable");
            assert!(
                matches!(e, Error::Terminal(_)),
                "status {s} should be terminal"
            );
        }
    }

    #[test]
    fn auth_is_terminal() {
        for (s, code) in [(401u16, "UNAUTHORIZED"), (403, "FORBIDDEN")] {
            let e = classify(s, code);
            assert!(!e.is_retryable());
            assert!(matches!(e, Error::Terminal(TerminalError::Auth { .. })));
            assert_eq!(e.exit_code(), 77);
        }
        // An auth code returned with a 400 still classifies as Auth.
        assert!(matches!(
            classify(400, "SIGNATURE_INVALID"),
            Error::Terminal(TerminalError::Auth { .. })
        ));
    }

    #[test]
    fn insufficient_funds_maps_from_both_conventions() {
        for code in ["INSUFFICIENT_BALANCE", "InsufficientMargin"] {
            assert!(
                matches!(
                    classify(400, code),
                    Error::Terminal(TerminalError::InsufficientFunds { .. })
                ),
                "{code} should be InsufficientFunds"
            );
        }
    }

    #[test]
    fn insufficient_substring_does_not_over_match() {
        // Codes that merely *contain* "INSUFFICIENT" but aren't a balance/margin
        // shortfall must not surface as InsufficientFunds — they're really
        // auth/permission or generic bad-request errors.
        assert!(
            matches!(
                classify(403, "INSUFFICIENT_PERMISSIONS"),
                Error::Terminal(TerminalError::Auth { .. })
            ),
            "INSUFFICIENT_PERMISSIONS at 403 should be Auth, not InsufficientFunds"
        );
        assert!(
            matches!(
                classify(400, "INSUFFICIENT_PRIVILEGES"),
                Error::Terminal(TerminalError::BadRequest { .. })
            ),
            "INSUFFICIENT_PRIVILEGES at 400 should fall through to BadRequest"
        );
    }

    #[test]
    fn order_rejects_vs_bad_request() {
        assert!(matches!(
            classify(400, "InvalidTickSize"),
            Error::Terminal(TerminalError::InvalidOrder { .. })
        ));
        // Request-shape errors stay BadRequest.
        for code in ["InvalidBody", "InvalidQuery", "BAD_REQUEST", "NOT_FOUND"] {
            assert!(
                matches!(
                    classify(400, code),
                    Error::Terminal(TerminalError::BadRequest { .. })
                ),
                "{code} should be BadRequest"
            );
        }
        // Terminal but non-auth → EX_DATAERR.
        assert_eq!(classify(400, "InvalidTickSize").exit_code(), 65);
    }

    #[test]
    fn decode_error_is_terminal_not_retryable() {
        let serde_err = serde_json::from_str::<i32>("\"not a number\"").unwrap_err();
        let e: Error = serde_err.into();
        assert!(!e.is_retryable());
        assert!(matches!(e, Error::Terminal(TerminalError::Decode(_))));
    }

    #[test]
    fn local_validation_errors_are_terminal() {
        // Local credential and request-validation failures never leave the
        // client, so they are terminal and carry the auth-style exit code only
        // for credentials.
        let creds = Error::credentials("missing credentials");
        assert!(!creds.is_retryable());
        assert!(matches!(
            creds,
            Error::Terminal(TerminalError::Credentials(_))
        ));
        assert_eq!(creds.exit_code(), 77);

        let req = Error::invalid_request("leverage must be at least 1");
        assert!(!req.is_retryable());
        assert!(matches!(
            req,
            Error::Terminal(TerminalError::InvalidRequest(_))
        ));
        assert_eq!(req.exit_code(), 65);
    }

    #[test]
    fn websocket_failures_are_transient() {
        // A surfaced WS transport/protocol error or a closed stream is
        // recoverable by reconnecting — classify as transient.
        let closed: Error = TransientError::StreamClosed.into();
        assert!(closed.is_retryable());
        let lagged: Error = TransientError::Lagged { dropped: 3 }.into();
        assert!(lagged.is_retryable());
    }
}
