//! Request authentication — a thin signer that mirrors the Exchange API's
//! schemes and nothing more.
//!
//! Three concerns, each behind a small abstraction so callers can swap pieces
//! without touching the client:
//!
//! - [`Credential`] — the trait the client calls to authenticate a REST
//!   request. The built-in [`Credentials`] enum implements it for the two
//!   header schemes the API ships:
//!   - [`Credentials::ApiKey`] — HMAC-SHA256 request signing (`X-API-Key` /
//!     `X-Timestamp` / `X-Signature`), the scheme used for trading.
//!   - [`Credentials::Session`] — a bearer token from `POST /auth/login`, used
//!     only for `/keys` management.
//! - [`Nonce`] — the source of the millisecond timestamp stamped on each signed
//!   request. Defaults to [`SystemTimeNonce`]; pluggable for clock-skew
//!   correction or deterministic tests.
//! - [`EthSigner`] — an EVM wallet key that produces the EIP-191 `signIn` and
//!   EIP-712 `registerAgent` payloads.
//!
//! Every secret lives in a [`secrecy::SecretString`], and this module signs —
//! it never stores sessions, refreshes tokens, or otherwise manages state.

mod eth;

pub use eth::{AgentRegistration, EthSigner, LoginRequest, SIGN_IN_MESSAGE};

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Error, Result};
use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

/// The canonical parts of an outbound request that a [`Credential`] signs.
///
/// `query` is the exact percent-encoded query string without the leading `?`
/// (empty when there is none), `body` the raw request body (empty for bodyless
/// methods), and `timestamp_ms` the [`Nonce`] value for this request.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct SigningContext<'a> {
    /// HTTP method, e.g. `GET` (case-insensitive; upper-cased when signed).
    pub method: &'a str,
    /// Request path, e.g. `/orders`.
    pub path: &'a str,
    /// Exact encoded query string without `?` (empty if none).
    pub query: &'a str,
    /// Raw request body (empty for bodyless methods).
    pub body: &'a [u8],
    /// Millisecond timestamp/nonce for this request.
    pub timestamp_ms: u64,
}

/// A credential that authenticates REST requests by contributing headers.
///
/// Implement this to plug in a custom scheme (e.g. an agent-key signer or an
/// HSM-backed HMAC); the built-in [`Credentials`] covers the API's own schemes.
/// Implementations must be cheap to call and free of side effects — the client
/// may invoke [`auth_headers`](Credential::auth_headers) once per request,
/// including on retries.
pub trait Credential: fmt::Debug + Send + Sync {
    /// Produce the authentication headers for `ctx`, as `(name, value)` pairs.
    fn auth_headers(&self, ctx: &SigningContext<'_>) -> Result<Vec<(&'static str, String)>>;
}

/// Source of the millisecond timestamp stamped on each signed request to make
/// it unique and replay-resistant.
///
/// The HMAC scheme requires this value to be within 30 seconds of server time,
/// so the default [`SystemTimeNonce`] is almost always correct; override it only
/// to correct for clock skew or to make signing deterministic under test. Values
/// need not be monotonic, but a custom source must stay inside that window.
pub trait Nonce: fmt::Debug + Send + Sync {
    /// Return the next timestamp/nonce, in Unix milliseconds.
    fn next(&self) -> u64;
}

/// Default [`Nonce`]: the system clock in Unix milliseconds.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct SystemTimeNonce;

impl Nonce for SystemTimeNonce {
    fn next(&self) -> u64 {
        // A clock before the epoch is nonsensical; clamp to 0 rather than panic
        // so a misconfigured host degrades to a rejected request, not a crash.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

/// API credentials for authenticated requests — the built-in [`Credential`]
/// implementations for the API's two header schemes.
#[derive(Debug)]
#[non_exhaustive]
pub enum Credentials {
    /// HMAC API key: `key_id` plus the 32-byte hex `secret` from `POST /keys`.
    ApiKey {
        /// Public key identifier (`X-API-Key`).
        key_id: String,
        /// 32-byte secret, hex-encoded.
        secret: SecretString,
    },
    /// A session bearer token from `POST /auth/login`.
    Session {
        /// The bearer token.
        token: SecretString,
    },
}

impl Credentials {
    /// HMAC API-key credentials. `secret` is the 64-char hex string returned by
    /// `POST /keys`.
    pub fn api_key(key_id: impl Into<String>, secret: impl Into<String>) -> Self {
        Credentials::ApiKey {
            key_id: key_id.into(),
            secret: SecretString::from(secret.into()),
        }
    }

    /// Session bearer-token credentials.
    pub fn session(token: impl Into<String>) -> Self {
        Credentials::Session {
            token: SecretString::from(token.into()),
        }
    }

    /// Box these credentials as a trait object for [`Config`](crate::Config).
    pub fn into_arc(self) -> Arc<dyn Credential> {
        Arc::new(self)
    }
}

impl Credential for Credentials {
    /// Build the auth headers for a request.
    ///
    /// The HMAC canonical string is
    /// `{ts}\n{METHOD}\n{path}\n{query}\n{sha256_hex(body)}`, matching the
    /// server's `hmacAuth` definition byte-for-byte.
    fn auth_headers(&self, ctx: &SigningContext<'_>) -> Result<Vec<(&'static str, String)>> {
        match self {
            Credentials::ApiKey { key_id, secret } => {
                let secret_bytes = hex::decode(secret.expose_secret().trim_start_matches("0x"))
                    .map_err(|_| Error::Auth("API key secret must be hex".into()))?;
                let body_hash = hex::encode(Sha256::digest(ctx.body));
                let ts = ctx.timestamp_ms.to_string();
                let canonical = format!(
                    "{ts}\n{}\n{}\n{}\n{body_hash}",
                    ctx.method.to_ascii_uppercase(),
                    ctx.path,
                    ctx.query,
                );
                let mut mac = Hmac::<Sha256>::new_from_slice(&secret_bytes)
                    .map_err(|_| Error::Auth("invalid HMAC key".into()))?;
                mac.update(canonical.as_bytes());
                let signature = hex::encode(mac.finalize().into_bytes());
                Ok(vec![
                    ("x-api-key", key_id.clone()),
                    ("x-timestamp", ts),
                    ("x-signature", signature),
                ])
            }
            Credentials::Session { token } => Ok(vec![(
                "authorization",
                format!("Bearer {}", token.expose_secret()),
            )]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        method: &'a str,
        path: &'a str,
        query: &'a str,
        body: &'a [u8],
    ) -> SigningContext<'a> {
        SigningContext {
            method,
            path,
            query,
            body,
            timestamp_ms: 1_776_033_900_000,
        }
    }

    // Golden vector cross-checked against the indexer's `verify_hmac`
    // canonical (`{ts}\nGET\n/keys\n\n{sha256_hex("")}`).
    #[test]
    fn hmac_signature_matches_golden_vector() {
        let creds = Credentials::api_key(
            "nx_test",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        );
        let headers = creds.auth_headers(&ctx("GET", "/keys", "", b"")).unwrap();
        let get = |k: &str| headers.iter().find(|(hk, _)| *hk == k).unwrap().1.clone();
        assert_eq!(get("x-api-key"), "nx_test");
        assert_eq!(get("x-timestamp"), "1776033900000");
        assert_eq!(
            get("x-signature"),
            "44cd3a44cd884cfc455ea66124ad06b9e6f4b701fcce692dd772b29096ea3e4e"
        );
    }

    // Golden vector for a NON-EMPTY query, cross-checked against an independent
    // HMAC-SHA256 of the canonical `{ts}\nGET\n/orders\n{query}\n{sha256_hex("")}`.
    #[test]
    fn hmac_signature_matches_golden_vector_with_query() {
        let query = serde_urlencoded::to_string([("limit", "50"), ("cursor", "abc")]).unwrap();
        assert_eq!(query, "limit=50&cursor=abc");

        let creds = Credentials::api_key(
            "nx_test",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        );
        let headers = creds
            .auth_headers(&ctx("GET", "/orders", &query, b""))
            .unwrap();
        let get = |k: &str| headers.iter().find(|(hk, _)| *hk == k).unwrap().1.clone();
        assert_eq!(get("x-api-key"), "nx_test");
        assert_eq!(get("x-timestamp"), "1776033900000");
        assert_eq!(
            get("x-signature"),
            "87b7a9ba5e28360dafe1e26d6c9bb28ae33ba399a60f6bd52e7b6551d997129e"
        );
    }

    #[test]
    fn session_sets_bearer() {
        let creds = Credentials::session("tok123");
        let headers = creds
            .auth_headers(&ctx("GET", "/account", "", b""))
            .unwrap();
        assert_eq!(
            headers,
            vec![("authorization", "Bearer tok123".to_string())]
        );
    }

    #[test]
    fn malformed_hmac_secret_is_rejected() {
        let creds = Credentials::api_key("nx_test", "not-hex");
        assert!(matches!(
            creds.auth_headers(&ctx("GET", "/keys", "", b"")),
            Err(Error::Auth(_))
        ));
    }

    #[test]
    fn system_time_nonce_is_in_ms_range() {
        // Sanity: a real clock yields a 13-digit ms timestamp (> year 2001).
        assert!(SystemTimeNonce.next() > 1_000_000_000_000);
    }
}
