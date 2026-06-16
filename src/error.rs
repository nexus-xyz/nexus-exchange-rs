//! Error types.

use thiserror::Error;

/// Errors returned by the SDK.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Transport or HTTP-layer failure.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// The API returned a structured error envelope (`{ code, message }`).
    #[error("api error [{code}]: {message}")]
    Api {
        /// Machine-readable error code.
        code: String,
        /// Human-readable message.
        message: String,
    },
}
