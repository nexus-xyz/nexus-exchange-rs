//! Agent-key request signing — the `x-agent` / `x-timestamp` / `x-nonce` /
//! `x-signature` scheme.
//!
//! An agent key is a secp256k1 keypair registered to a wallet with
//! `POST /agents/register` (see [`EthSigner::register_agent`](super::EthSigner::register_agent)).
//! Once registered it signs trading requests on the wallet's behalf.
//! [`AgentSigner`] is the [`Credential`] that does that signing.
//!
//! # Wire format
//!
//! This is a port of the server's verifier, not a design. The canonical string
//! is six LF-separated fields:
//!
//! ```text
//! {METHOD-uppercase}\n{path}\n{query}\n{hex(sha256(body))}\n{timestamp_ms}\n{nonce}
//! ```
//!
//! Note that the order differs from the HMAC scheme: the method comes first and
//! the timestamp is near the end. `query` is the exact encoded query string
//! without the leading `?`, or an empty line when there is none. The prehash is
//! `keccak256(canonical)`, with **no** EIP-191 prefix. The signature is a
//! recoverable secp256k1 ECDSA signature, sent as `0x`-prefixed 65-byte
//! `r||s||v` hex with `v ∈ {27, 28}`. It is deterministic (RFC 6979) and low-S,
//! and the server rejects high-S signatures.
//!
//! The server checks, in order:
//!
//! 1. `x-timestamp` is within ±30 s of its clock.
//! 2. The signature recovers to `x-agent`.
//! 3. The agent is registered and not expired.
//! 4. On mutating methods, `x-nonce` is strictly greater than the highest nonce
//!    it has seen for that agent.
//!
//! The known-answer vectors in this module's tests were produced by the
//! exchange frontend's own signer (`@noble/curves`) and checked independently
//! with `eth-account`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use secrecy::SecretString;
use sha2::Sha256;
use sha3::{Digest, Keccak256};

use super::eth::{address_of, sign_prehash, signing_key, to_hex_address};
use super::{Credential, SigningContext};
use crate::Result;

/// Signs REST requests with a registered agent key: the `x-agent`,
/// `x-timestamp`, `x-nonce` and `x-signature` headers.
///
/// Build one from the agent's 32-byte hex private key with
/// [`AgentSigner::from_hex`], register [`address`](AgentSigner::address) with
/// `POST /agents/register`, and install it with
/// [`Config::agent_key`](crate::Config::agent_key) (or
/// [`Config::with_credential`](crate::Config::with_credential)).
///
/// # Nonces
///
/// The server requires each agent's nonce to strictly increase on mutating
/// requests. A signer issues `max(previous + 1, timestamp_ms)`. That value is
/// strictly increasing within one signer and roughly tracks the wall clock, so
/// a restarted process picks up above the nonces it issued before.
///
/// Some cases can still produce a rejected nonce:
///
/// - **Concurrent writes from one signer.** Requests signed in nonce order can
///   reach the server in a different order. The server then rejects the lower
///   nonce as a replay with a `401`. If you send mutating requests
///   concurrently and cannot absorb that, serialize them.
/// - **One agent key in several processes.** The processes' nonces can
///   collide. Register one agent per process instead.
///
/// Reads (`GET`) do not consume a nonce on the server, so they are unaffected.
#[derive(Debug)]
pub struct AgentSigner {
    /// 32-byte secp256k1 private key, hex-encoded.
    key: SecretString,
    /// Derived 20-byte agent address.
    address: [u8; 20],
    /// Highest nonce issued so far (0 before the first request).
    last_nonce: AtomicU64,
}

impl AgentSigner {
    /// Build an agent signer from a 32-byte hex private key (`0x`-prefix
    /// optional).
    ///
    /// Returns [`crate::TerminalError::Credentials`] if the key is not 32 bytes
    /// of valid hex, or is not a valid secp256k1 scalar.
    pub fn from_hex(private_key: impl Into<String>) -> Result<Self> {
        let key = SecretString::from(private_key.into());
        let address = address_of(&signing_key(&key)?);
        Ok(Self {
            key,
            address,
            last_nonce: AtomicU64::new(0),
        })
    }

    /// The agent's address as lowercase `0x`-prefixed hex. This is the value
    /// to register with `POST /agents/register` and the value sent as
    /// `x-agent`.
    pub fn address(&self) -> String {
        to_hex_address(&self.address)
    }

    /// Box this signer as a trait object for [`Config`](crate::Config).
    pub fn into_arc(self) -> Arc<dyn Credential> {
        Arc::new(self)
    }

    /// Issue the next nonce, `max(last + 1, floor)`. The update is atomic, so
    /// concurrent callers always get distinct, increasing values.
    fn next_nonce(&self, floor: u64) -> u64 {
        let prev = self
            .last_nonce
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |last| {
                Some(last.saturating_add(1).max(floor))
            })
            // The closure always returns `Some`, so this cannot fail.
            .unwrap_or_else(|last| last);
        prev.saturating_add(1).max(floor)
    }

    /// Sign `ctx` with an explicit `nonce`. This is deterministic, which lets
    /// the tests pin the output against known-answer vectors.
    fn headers_with_nonce(
        &self,
        ctx: &SigningContext<'_>,
        nonce: u64,
    ) -> Result<Vec<(&'static str, String)>> {
        let canonical = canonical_string(ctx, nonce);
        let digest: [u8; 32] = Keccak256::digest(canonical.as_bytes()).into();
        let signature = sign_prehash(&self.key, &digest)?;
        Ok(vec![
            ("x-agent", self.address()),
            ("x-timestamp", ctx.timestamp_ms.to_string()),
            ("x-nonce", nonce.to_string()),
            ("x-signature", signature),
        ])
    }
}

impl Credential for AgentSigner {
    /// Build the four agent-auth headers for a request. The nonce is issued by
    /// the signer (see [`AgentSigner`]); `ctx.timestamp_ms` is its floor.
    fn auth_headers(&self, ctx: &SigningContext<'_>) -> Result<Vec<(&'static str, String)>> {
        let nonce = self.next_nonce(ctx.timestamp_ms);
        self.headers_with_nonce(ctx, nonce)
    }
}

/// Build the agent-key canonical string:
/// `{METHOD}\n{path}\n{query}\n{hex(sha256(body))}\n{ts_ms}\n{nonce}`.
fn canonical_string(ctx: &SigningContext<'_>, nonce: u64) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{nonce}",
        ctx.method.to_ascii_uppercase(),
        ctx.path,
        ctx.query,
        hex::encode(Sha256::digest(ctx.body)),
        ctx.timestamp_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

    fn ctx<'a>(
        method: &'a str,
        path: &'a str,
        query: &'a str,
        body: &'a [u8],
        timestamp_ms: u64,
    ) -> SigningContext<'a> {
        SigningContext {
            method,
            path,
            query,
            body,
            timestamp_ms,
        }
    }

    fn get<'a>(headers: &'a [(&'static str, String)], name: &str) -> &'a str {
        &headers.iter().find(|(k, _)| *k == name).unwrap().1
    }

    // The server's own pinned vector (`exchange-sec-utils::signing`
    // `canonical_string_matches_the_pinned_wire_format`). Lowercase method
    // on input, uppercase on the wire.
    #[test]
    fn canonical_string_matches_server_pinned_vector() {
        assert_eq!(
            canonical_string(
                &ctx("post", "/account/withdraw", "a=1", b"hello", 1_700),
                42
            ),
            "POST\n/account/withdraw\na=1\n\
             2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\n1700\n42",
        );
    }

    /// One known-answer vector: `(key, method, path, query, body, ts, nonce)`
    /// maps to `(x-agent, x-signature)`.
    struct Vector {
        key: &'static str,
        method: &'static str,
        path: &'static str,
        query: &'static str,
        body: &'static str,
        ts: u64,
        nonce: u64,
        agent: &'static str,
        signature: &'static str,
    }

    // These were generated by the exchange frontend's signer
    // (`exchange-terminal/lib/agent/signing.ts :: signRequestHeaders`, on
    // `@noble/curves` 1.x). They were then independently reproduced
    // byte-for-byte with Python `eth-account` (`Account.unsafe_sign_hash` over
    // `keccak256(canonical)`). The cases cover an empty-body GET, a JSON POST
    // with a lowercase method, and a DELETE with a query, and they include both
    // recovery ids (v = 27 and v = 28).
    const VECTORS: &[Vector] = &[
        Vector {
            key: "0x0101010101010101010101010101010101010101010101010101010101010101",
            method: "GET",
            path: "/account/summary",
            query: "",
            body: "",
            ts: 1_776_033_900_000,
            nonce: 1_776_033_900_000,
            agent: "0x1a642f0e3c3af545e7acbd38b07251b3990914f1",
            signature: "0xd94b40dff9c3d0e0a649178b6eb3159d9a3522a1ccc66390b42a7a444b8e52c8\
                        5f2d2a9d66e8a48ac5040d8e96f526d352b3b7a686634280b9192eb6c12a61b61b",
        },
        Vector {
            key: "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318",
            method: "post",
            path: "/orders",
            query: "",
            body: r#"{"market_id":"BTC-USDX-PERP","side":"Buy","order_type":"Limit","quantity":"0.1","price":"50000"}"#,
            ts: 1_776_033_900_123,
            nonce: 42,
            agent: "0x2c7536e3605d9c16a7a3d7b1898e529396a65c23",
            signature: "0xeef09105834062da10d208be38311995d04cbc5e05043b89e7cbe6d4497d11b5\
                        1f61729639551a8cfb6ea3e4fe0765afeba7bf3b5f5e09f63bc095c89005b61d1c",
        },
        Vector {
            key: "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318",
            method: "DELETE",
            path: "/orders",
            query: "market_id=BTC-USDX-PERP",
            body: "",
            ts: 1_776_033_900_456,
            nonce: 43,
            agent: "0x2c7536e3605d9c16a7a3d7b1898e529396a65c23",
            signature: "0x002554d4216daa303818dfcc5a15eb86940691e11ad6ab25a270916ec5a43a1a\
                        5eb4c18dd898aa3ebd099c1683337e3c545417c2b67a0d384f1d0355ffa807f91c",
        },
    ];

    #[test]
    fn signatures_match_reference_implementation_vectors() {
        for v in VECTORS {
            let signer = AgentSigner::from_hex(v.key).unwrap();
            assert_eq!(signer.address(), v.agent);
            let c = ctx(v.method, v.path, v.query, v.body.as_bytes(), v.ts);
            let headers = signer.headers_with_nonce(&c, v.nonce).unwrap();
            assert_eq!(get(&headers, "x-agent"), v.agent);
            assert_eq!(get(&headers, "x-timestamp"), v.ts.to_string());
            assert_eq!(get(&headers, "x-nonce"), v.nonce.to_string());
            assert_eq!(
                get(&headers, "x-signature"),
                v.signature,
                "{} {}",
                v.method,
                v.path
            );
        }
    }

    // This mirrors the server's check: ecrecover over `keccak256(canonical)`
    // must yield `x-agent`. It fails if an EIP-191 prefix creeps into the
    // prehash.
    #[test]
    fn signature_recovers_to_agent_over_unprefixed_keccak() {
        let signer = AgentSigner::from_hex(VECTORS[1].key).unwrap();
        let c = ctx("POST", "/orders", "", b"{}", 1_776_033_900_000);
        let headers = signer.headers_with_nonce(&c, 7).unwrap();
        let sig = hex::decode(get(&headers, "x-signature").trim_start_matches("0x")).unwrap();
        assert_eq!(sig.len(), 65);
        assert!(sig[64] == 27 || sig[64] == 28);
        let signature = Signature::from_slice(&sig[..64]).unwrap();
        assert!(signature.normalize_s().is_none(), "must be low-S");
        let digest: [u8; 32] = Keccak256::digest(canonical_string(&c, 7).as_bytes()).into();
        let vk = VerifyingKey::recover_from_prehash(
            &digest,
            &signature,
            RecoveryId::from_byte(sig[64] - 27).unwrap(),
        )
        .unwrap();
        let point = vk.to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        assert_eq!(format!("0x{}", hex::encode(&hash[12..])), signer.address());
    }

    #[test]
    fn nonce_starts_at_timestamp_and_strictly_increases() {
        let signer = AgentSigner::from_hex(VECTORS[0].key).unwrap();
        let nonce = |ts| {
            let h = signer
                .auth_headers(&ctx("POST", "/orders", "", b"", ts))
                .unwrap();
            get(&h, "x-nonce").parse::<u64>().unwrap()
        };
        assert_eq!(nonce(1_000), 1_000);
        // Two requests in the same millisecond still get distinct nonces.
        assert_eq!(nonce(1_000), 1_001);
        // A clock that steps backwards does not regress the nonce.
        assert_eq!(nonce(500), 1_002);
        // Once the clock moves past the counter, the nonce follows the clock.
        assert_eq!(nonce(5_000), 5_000);
    }

    #[test]
    fn concurrent_nonces_are_unique() {
        let signer = Arc::new(AgentSigner::from_hex(VECTORS[0].key).unwrap());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = Arc::clone(&signer);
                std::thread::spawn(move || (0..100).map(|_| s.next_nonce(1)).collect::<Vec<_>>())
            })
            .collect();
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 800);
    }

    #[test]
    fn invalid_key_is_rejected() {
        for bad in ["not-hex", "0x1234", &"00".repeat(32)] {
            assert!(matches!(
                AgentSigner::from_hex(bad),
                Err(crate::Error::Terminal(crate::TerminalError::Credentials(_)))
            ));
        }
    }

    #[test]
    fn debug_does_not_leak_the_key() {
        let signer = AgentSigner::from_hex(VECTORS[0].key).unwrap();
        let dbg = format!("{signer:?}");
        assert!(!dbg.contains("0101010101010101"), "{dbg}");
    }
}
