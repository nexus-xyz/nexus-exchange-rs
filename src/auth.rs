//! Request authentication.
//!
//! Two thin credential types, mirroring the API's schemes:
//! - [`Credentials::ApiKey`] — HMAC-SHA256 request signing (`x-api-key` /
//!   `x-timestamp` / `x-signature`), the scheme used by programmatic clients.
//! - [`Credentials::Session`] — a bearer token from `POST /auth/login`.
//!
//! Agent keys (EIP-712 / secp256k1) are a planned follow-up.

use crate::{Error, Result};
use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

/// API credentials for authenticated requests.
#[derive(Debug)]
#[non_exhaustive]
pub enum Credentials {
    /// HMAC API key: `key_id` plus the 32-byte hex `secret` from `POST /keys`.
    ApiKey {
        /// Public key identifier (`x-api-key`).
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

    /// Build the auth headers for a request.
    ///
    /// `path` is the API path (e.g. `/keys`), `query` the exact encoded query
    /// string without `?`, and `body` the raw request body (empty for GET).
    /// The HMAC canonical string is
    /// `{ts}\n{METHOD}\n{path}\n{query}\n{sha256_hex(body)}`.
    pub(crate) fn headers(
        &self,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
        ts_ms: u64,
    ) -> Result<Vec<(&'static str, String)>> {
        match self {
            Credentials::ApiKey { key_id, secret } => {
                let secret_bytes = hex::decode(secret.expose_secret().trim_start_matches("0x"))
                    .map_err(|_| Error::Auth("API key secret must be hex".into()))?;
                let body_hash = hex::encode(Sha256::digest(body));
                let ts = ts_ms.to_string();
                let canonical = format!(
                    "{ts}\n{}\n{path}\n{query}\n{body_hash}",
                    method.to_ascii_uppercase()
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

    // Golden vector cross-checked against the indexer's `verify_hmac`
    // canonical (`{ts}\nGET\n/keys\n\n{sha256_hex("")}`).
    #[test]
    fn hmac_signature_matches_golden_vector() {
        let creds = Credentials::api_key(
            "nx_test",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        );
        let headers = creds
            .headers("GET", "/keys", "", b"", 1_776_033_900_000)
            .unwrap();
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
    //
    // `signed_get` signs the `serde_urlencoded` output and embeds that exact
    // string in the URL (see `client::Client::signed_get`), so this asserts two
    // things at once: (1) the signature is correct for a populated query, and
    // (2) `serde_urlencoded` renders the pairs to the byte-for-byte string we
    // signed — `limit=50&cursor=abc` — so signed === sent holds once a signed
    // GET starts passing params. (Today every `signed_get` passes `&[]`.)
    #[test]
    fn hmac_signature_matches_golden_vector_with_query() {
        // The exact query string a signed GET would sign and send.
        let query = serde_urlencoded::to_string([("limit", "50"), ("cursor", "abc")]).unwrap();
        assert_eq!(query, "limit=50&cursor=abc");

        let creds = Credentials::api_key(
            "nx_test",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        );
        let headers = creds
            .headers("GET", "/orders", &query, b"", 1_776_033_900_000)
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
        let headers = creds.headers("GET", "/account", "", b"", 0).unwrap();
        assert_eq!(
            headers,
            vec![("authorization", "Bearer tok123".to_string())]
        );
    }
}
