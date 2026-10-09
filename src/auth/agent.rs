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
//!
//! # Signed trading actions
//!
//! On a target with a [deployment domain](crate::Network::deployment_domain),
//! the eight order-path routes sign their EIP-712 trading struct instead (spec
//! "Signed trading actions"): `x-agent`, `x-action-signature`,
//! `x-action-timestamp` and `x-action-nonce`, plus `x-acting-account` when the
//! action is for another account. No `x-signature`, `x-timestamp` or
//! `x-nonce`: the server picks the format from which headers are present. The
//! struct names the account it acts on, so the signer must know it; see
//! [`AgentSigner::with_account`]. Every other request keeps the canonical
//! string, and both formats draw from the one nonce sequence below, as the
//! server keeps one per agent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use secrecy::SecretString;
use sha2::Sha256;
use sha3::{Digest, Keccak256};

use super::eth::{address_of, parse_address, sign_prehash, signing_key, to_hex_address};
use super::trading::{self, Envelope};
use super::{Credential, SigningContext, WriteQueue};
use crate::{Error, Result};

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
/// Nonces are issued in order, but concurrent requests could still reach the
/// server out of order, and the server would reject the lower nonce as a
/// replay with a `401`. To prevent that, the client sends a signer's mutating
/// requests (`POST`, `PUT`, `PATCH`, `DELETE`) one at a time, first come first
/// served: each one is signed only when its turn comes and holds the signer's
/// queue until its response arrives (or [`Config::with_timeout`](crate::Config::with_timeout)
/// expires). You can call write methods concurrently from one signer.
///
/// That has two costs:
///
/// - **Write throughput per agent key is one round trip at a time.** Register
///   more agents if one key is not enough.
/// - **Cancels wait in the same queue as order placements.** The server keeps
///   one nonce sequence per agent across every mutating method, so a separate
///   cancel lane would bring the replay rejections back. A cancel issued
///   behind a slow placement waits for it.
///
/// Reads (`GET`) do not consume a nonce on the server, so they skip the queue.
///
/// One case can still produce a rejected nonce: **one agent key in several
/// processes** (or several `AgentSigner`s built from one key). Each has its own
/// counter and queue, so their nonces can collide or interleave. Register one
/// agent per process instead.
#[derive(Debug)]
pub struct AgentSigner {
    /// 32-byte secp256k1 private key, hex-encoded.
    key: SecretString,
    /// Derived 20-byte agent address.
    address: [u8; 20],
    /// Highest nonce issued so far (0 before the first request).
    last_nonce: AtomicU64,
    /// Orders this signer's mutating requests; see the type docs.
    queue: WriteQueue,
    /// The account that registered this agent, which a trading struct names.
    account: Option<[u8; 20]>,
    /// The account to trade instead, e.g. a subaccount (`x-acting-account`).
    acting_account: Option<[u8; 20]>,
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
            queue: WriteQueue::new(),
            account: None,
            acting_account: None,
        })
    }

    /// Name the account that registered this agent (the owner wallet,
    /// `0x`-prefixed hex). A signed trading action names the account it acts
    /// on, so the eight order-path routes need it on a target with a
    /// [deployment domain](crate::Network::deployment_domain).
    ///
    /// Returns [`crate::TerminalError::InvalidRequest`] if `account` is not a
    /// 20-byte hex address.
    pub fn with_account(mut self, account: &str) -> Result<Self> {
        self.account = Some(parse_account(account)?);
        Ok(self)
    }

    /// Trade `account` instead of the agent's own, e.g. a subaccount of it.
    /// Signed trading actions then name `account` and send it as
    /// `x-acting-account`.
    ///
    /// The server refuses an action for another account with `403
    /// ActingAccountUnverified` until it enforces signed actions.
    pub fn with_acting_account(mut self, account: &str) -> Result<Self> {
        self.acting_account = Some(parse_account(account)?);
        Ok(self)
    }

    /// The signed-action headers for `ctx`, or `None` when `ctx` is not one of
    /// the eight trading routes or the target declares no deployment domain.
    /// Draws a nonce only when it signs.
    pub(crate) fn action_headers(
        &self,
        ctx: &SigningContext<'_>,
    ) -> Result<Option<Vec<(&'static str, String)>>> {
        if ctx.deployment_domain.is_none() || !trading::is_trading_route(ctx.method, ctx.path) {
            return Ok(None);
        }
        let nonce = self.next_nonce(ctx.timestamp_ms);
        self.action_headers_with_nonce(ctx, nonce)
    }

    /// [`action_headers`](Self::action_headers) with an explicit nonce.
    fn action_headers_with_nonce(
        &self,
        ctx: &SigningContext<'_>,
        nonce: u64,
    ) -> Result<Option<Vec<(&'static str, String)>>> {
        let Some(domain) = ctx.deployment_domain else {
            return Ok(None);
        };
        let account = self.acting_account.or(self.account).ok_or_else(|| {
            Error::credentials(
                "a signed trading action names the account it acts on: call \
                 AgentSigner::with_account with the wallet that registered this agent",
            )
        })?;
        let envelope = Envelope {
            account,
            domain,
            timestamp_ms: ctx.timestamp_ms,
            nonce,
        };
        let Some(digest) =
            trading::request_digest(ctx.method, ctx.path, ctx.query, ctx.body, &envelope)?
        else {
            return Ok(None);
        };
        let mut headers = vec![
            ("x-action-signature", sign_prehash(&self.key, &digest)?),
            ("x-action-timestamp", ctx.timestamp_ms.to_string()),
            ("x-action-nonce", nonce.to_string()),
        ];
        if self.acting_account.is_some() && self.acting_account != self.account {
            headers.push(("x-acting-account", to_hex_address(&account)));
        }
        Ok(Some(headers))
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
        // `try_update`, the suggested replacement, is newer than the 1.86 MSRV.
        #[allow(deprecated)]
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
    /// On a trading route of a target with a deployment domain, `x-agent`
    /// plus the signed action instead (see the module docs).
    fn auth_headers(&self, ctx: &SigningContext<'_>) -> Result<Vec<(&'static str, String)>> {
        if let Some(action) = self.action_headers(ctx)? {
            let mut headers = vec![("x-agent", self.address())];
            headers.extend(action);
            return Ok(headers);
        }
        let nonce = self.next_nonce(ctx.timestamp_ms);
        self.headers_with_nonce(ctx, nonce)
    }

    fn write_queue(&self) -> Option<&WriteQueue> {
        Some(&self.queue)
    }
}

fn parse_account(account: &str) -> Result<[u8; 20]> {
    parse_address(account)
        .map_err(|_| Error::invalid_request("account must be a 20-byte hex address"))
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
            deployment_domain: None,
        }
    }

    fn get<'a>(headers: &'a [(&'static str, String)], name: &str) -> &'a str {
        &headers.iter().find(|(k, _)| *k == name).unwrap().1
    }

    // Adding the write queue must not drop an auto trait from the public type
    // (cargo-semver-checks `auto_trait_impl_removed`).
    #[test]
    fn agent_signer_keeps_its_auto_traits() {
        fn assert_auto<T: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        assert_auto::<AgentSigner>();
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

    const OWNER: &str = "0x1111111111111111111111111111111111111111";
    const LIMIT_BODY: &str = r#"{"market_id":"BTC-USDX-PERP","side":"Buy","order_type":"Limit","price":"65000.5","quantity":"0.25","time_in_force":"GTC","stp":"CancelNewest","client_id":"order-1","max_slippage_bps":50}"#;

    fn typed_ctx<'a>(method: &'a str, path: &'a str, body: &'a [u8]) -> SigningContext<'a> {
        SigningContext {
            deployment_domain: Some("prd-testnet"),
            ..ctx(method, path, "", body, 1_700_000_000_000)
        }
    }

    /// The signed action recovers to the agent over the server's pinned
    /// `PlaceOrder` digest (`trading_intent.rs`, same envelope), and carries
    /// none of the canonical-string headers, which would select that format.
    #[test]
    fn trading_route_signs_the_pinned_struct() {
        let signer = AgentSigner::from_hex(VECTORS[1].key)
            .unwrap()
            .with_account(OWNER)
            .unwrap();
        let c = typed_ctx("POST", "/orders", LIMIT_BODY.as_bytes());
        let headers = signer.action_headers_with_nonce(&c, 7).unwrap().unwrap();
        assert_eq!(get(&headers, "x-action-timestamp"), "1700000000000");
        assert_eq!(get(&headers, "x-action-nonce"), "7");
        assert!(headers.iter().all(|(k, _)| *k != "x-acting-account"));
        let digest: [u8; 32] =
            hex::decode("15c5dd8665e1f92b194fb0b4932c561532a5100da669f8dbd402b8335fd58c7b")
                .unwrap()
                .try_into()
                .unwrap();
        let sig =
            hex::decode(get(&headers, "x-action-signature").trim_start_matches("0x")).unwrap();
        let signature = Signature::from_slice(&sig[..64]).unwrap();
        assert!(signature.normalize_s().is_none(), "must be low-S");
        let vk = VerifyingKey::recover_from_prehash(
            &digest,
            &signature,
            RecoveryId::from_byte(sig[64] - 27).unwrap(),
        )
        .unwrap();
        assert_eq!(address_of_key(&vk), signer.address());

        let all = signer.auth_headers(&c).unwrap();
        let names: Vec<_> = all.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            names,
            [
                "x-agent",
                "x-action-signature",
                "x-action-timestamp",
                "x-action-nonce"
            ]
        );
    }

    fn address_of_key(vk: &VerifyingKey) -> String {
        let point = vk.to_encoded_point(false);
        format!(
            "0x{}",
            hex::encode(&Keccak256::digest(&point.as_bytes()[1..])[12..])
        )
    }

    #[test]
    fn other_routes_and_undeclared_domains_keep_the_canonical_string() {
        let signer = AgentSigner::from_hex(VECTORS[1].key).unwrap();
        for c in [
            typed_ctx("POST", "/orders/preview", b"{}"),
            typed_ctx("GET", "/orders", b""),
            ctx(
                "POST",
                "/orders",
                "",
                LIMIT_BODY.as_bytes(),
                1_700_000_000_000,
            ),
        ] {
            let headers = signer.auth_headers(&c).unwrap();
            assert!(
                headers.iter().any(|(k, _)| *k == "x-signature"),
                "{} {}",
                c.method,
                c.path
            );
            assert!(headers.iter().all(|(k, _)| !k.starts_with("x-action")));
        }
    }

    #[test]
    fn a_trading_action_without_an_account_is_refused() {
        let signer = AgentSigner::from_hex(VECTORS[1].key).unwrap();
        let c = typed_ctx("DELETE", "/orders", b"");
        assert!(matches!(
            signer.auth_headers(&c),
            Err(crate::Error::Terminal(crate::TerminalError::Credentials(_)))
        ));
    }

    #[test]
    fn acting_for_a_subaccount_names_it() {
        let sub = "0x2222222222222222222222222222222222222222";
        let signer = AgentSigner::from_hex(VECTORS[1].key)
            .unwrap()
            .with_account(OWNER)
            .unwrap()
            .with_acting_account(sub)
            .unwrap();
        let c = typed_ctx("DELETE", "/orders", b"");
        let headers = signer.action_headers_with_nonce(&c, 7).unwrap().unwrap();
        assert_eq!(get(&headers, "x-acting-account"), sub);
        let own = AgentSigner::from_hex(VECTORS[1].key)
            .unwrap()
            .with_account(OWNER)
            .unwrap()
            .action_headers_with_nonce(&c, 7)
            .unwrap()
            .unwrap();
        assert_ne!(
            get(&headers, "x-action-signature"),
            get(&own, "x-action-signature"),
            "the acting account is signed"
        );
        assert!(AgentSigner::from_hex(VECTORS[1].key)
            .unwrap()
            .with_acting_account("0x1234")
            .is_err());
    }

    /// Both formats draw from one nonce sequence: the server keeps one per agent.
    #[test]
    fn typed_and_canonical_requests_share_the_nonce_sequence() {
        let signer = AgentSigner::from_hex(VECTORS[1].key)
            .unwrap()
            .with_account(OWNER)
            .unwrap();
        let typed = signer
            .auth_headers(&typed_ctx("DELETE", "/orders", b""))
            .unwrap();
        let canonical = signer
            .auth_headers(&ctx(
                "POST",
                "/orders/preview",
                "",
                b"{}",
                1_700_000_000_000,
            ))
            .unwrap();
        assert_eq!(get(&typed, "x-action-nonce"), "1700000000000");
        assert_eq!(get(&canonical, "x-nonce"), "1700000000001");
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
