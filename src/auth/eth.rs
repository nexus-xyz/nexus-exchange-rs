//! EVM wallet signing for the wallet-authorized flows: EIP-191 session login
//! (`signIn`), EIP-712 agent-key registration (`registerAgent`) and EIP-712
//! agent-key revocation (`revokeAgent`).
//!
//! [`EthSigner`] holds a secp256k1 private key in a [`SecretString`] and
//! produces the *signed requests* for those endpoints. It is a pure
//! signer: deterministic, side-effect free, and ignorant of the network — the
//! caller hands the result to the [`Client`](crate::Client) to send. Nonces and
//! expiries are caller-supplied so signing carries no hidden clock.

use crate::{Error, Network, Result};
use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use sha3::{Digest, Keccak256};
use zeroize::Zeroizing;

/// The exact, fixed message the API requires for EIP-191 session login.
pub const SIGN_IN_MESSAGE: &str = "Sign in to Nexus Exchange";

/// EIP-712 domain `name`, per the `/agents/register` spec. Re-exported through
/// [`Network::signing_domain`](crate::Network::signing_domain) so the value that
/// signs and the value the SDK advertises cannot drift apart.
pub(crate) const EIP712_DOMAIN_NAME: &str = "Nexus Exchange";
/// EIP-712 domain `version`, per the `/agents/register` spec. See
/// [`EIP712_DOMAIN_NAME`].
pub(crate) const EIP712_DOMAIN_VERSION: &str = "1";

/// The agent-management domain `salt` (`RegisterAgent`, `RevokeAgentKey`) for a
/// named network: `keccak256(network)`, exactly as the server derives it
/// (ENG-15643). Sourced by
/// [`Network::signing_domain`](crate::Network::signing_domain).
pub(crate) fn network_salt(network: &str) -> [u8; 32] {
    finalize32(Keccak256::new_with_prefix(network.as_bytes()))
}

/// Signed body for `POST /auth/login` (EIP-191 session login).
///
/// Produced by [`EthSigner::sign_in`]; hand its `signature` to
/// [`Client::login`](crate::Client::login).
#[derive(Debug, Clone, Serialize)]
pub struct LoginRequest {
    /// The signed message — always [`SIGN_IN_MESSAGE`].
    pub message: String,
    /// EIP-191 `personal_sign` signature, `0x`-prefixed (65 bytes).
    pub signature: String,
}

/// Signed body for `POST /agents/register` (EIP-712 agent registration).
///
/// Produced by [`EthSigner::register_agent`]; hand it to
/// [`Client::register_agent`](crate::Client::register_agent).
#[derive(Debug, Clone, Serialize)]
pub struct AgentRegistration {
    /// Owner wallet address (`0x`-prefixed), recovered from the signature.
    pub wallet: String,
    /// Agent address being registered (`0x`-prefixed).
    pub agent: String,
    /// Expiry as Unix milliseconds.
    pub expires_at: u64,
    /// Monotonic nonce.
    pub nonce: u64,
    /// EIP-712 signature over `RegisterAgent{agent, expiresAt, nonce}`,
    /// `0x`-prefixed (65 bytes).
    pub signature: String,
    /// Optional human-readable label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Signed `DELETE /agents/{address}` authorization (EIP-712 `RevokeAgentKey`).
///
/// Produced by [`EthSigner::revoke_agent`]; hand it to
/// [`Client::revoke_agent`](crate::Client::revoke_agent), which sends it as the
/// four `x-wallet-*` headers.
#[derive(Debug, Clone)]
pub struct AgentRevocation {
    /// Owner wallet address (`0x`-prefixed, lowercase), sent as
    /// `x-wallet-account`.
    pub account: String,
    /// Agent address being revoked (`0x`-prefixed, lowercase), the `{address}`
    /// path segment.
    pub agent: String,
    /// Unix-millisecond nonce, sent as `x-wallet-nonce`.
    pub nonce: u64,
    /// EIP-712 signature over `RevokeAgentKey{account, agent, nonce}`,
    /// `0x`-prefixed (65 bytes), sent as `x-wallet-signature`.
    pub signature: String,
    /// The domain `chainId` it was signed with, sent as `x-wallet-chain-id`.
    pub chain_id: u64,
}

/// An EVM wallet key that authorizes the wallet-signed auth flows.
///
/// Construct from a 32-byte hex private key with [`EthSigner::from_hex`]. The
/// key is validated and the Ethereum address derived once at construction; the
/// secret itself is kept in a [`SecretString`] and only decoded transiently
/// (into zeroized scratch) while signing.
#[derive(Debug)]
pub struct EthSigner {
    /// 32-byte secp256k1 private key, hex-encoded.
    key: SecretString,
    /// Derived 20-byte Ethereum address.
    address: [u8; 20],
}

impl EthSigner {
    /// Build a signer from a 32-byte hex private key (`0x`-prefix optional).
    ///
    /// Returns [`crate::TerminalError::Credentials`] if the key is not 32 bytes of valid hex or is not
    /// a valid secp256k1 scalar.
    pub fn from_hex(private_key: impl Into<String>) -> Result<Self> {
        let key = SecretString::from(private_key.into());
        let signing = signing_key(&key)?;
        let address = address_of(&signing);
        Ok(Self { key, address })
    }

    /// The wallet's Ethereum address, lowercase `0x`-prefixed hex.
    pub fn address(&self) -> String {
        to_hex_address(&self.address)
    }

    /// Sign the fixed login message ([`SIGN_IN_MESSAGE`]) with EIP-191
    /// `personal_sign`, yielding the `POST /auth/login` body.
    pub fn sign_in(&self) -> Result<LoginRequest> {
        let signature = self.sign_digest(&eip191_digest(SIGN_IN_MESSAGE.as_bytes()))?;
        Ok(LoginRequest {
            message: SIGN_IN_MESSAGE.to_string(),
            signature,
        })
    }

    /// Sign an agent-key registration with EIP-712, yielding the
    /// `POST /agents/register` body.
    ///
    /// `agent` is the agent keypair's address (`0x`-prefixed, 20 bytes).
    /// `expires_at_ms` and `nonce` are caller-supplied — the spec expects the
    /// expiry in `[now+1d, now+90d]` and suggests the current Unix-ms timestamp
    /// as a safe starting nonce. `chain_id` is the EIP-712 domain chain id (the
    /// exchange's testnet chain id); it is part of the signed payload, so it
    /// must match what the server verifies against.
    ///
    /// `network` is the network the registration is for. The server salts the
    /// `RegisterAgent` domain with `keccak256(network name)` (ENG-15643), so a
    /// registration verifies only on the network it was signed for. The salt
    /// is read from [`Network::signing_domain`]; a [`Network::Custom`] target
    /// names no network, has no salt, and is refused with
    /// [`crate::TerminalError::InvalidRequest`] rather than signed unsalted.
    pub fn register_agent(
        &self,
        agent: &str,
        expires_at_ms: u64,
        nonce: u64,
        chain_id: u64,
        network: &Network,
        label: Option<String>,
    ) -> Result<AgentRegistration> {
        let salt = agent_domain_salt(network, "RegisterAgent")?;
        let agent_addr = parse_address(agent)?;
        let digest = register_agent_digest(&agent_addr, expires_at_ms, nonce, chain_id, &salt);
        let signature = self.sign_digest(&digest)?;
        Ok(AgentRegistration {
            wallet: self.address(),
            agent: to_hex_address(&agent_addr),
            expires_at: expires_at_ms,
            nonce,
            signature,
            label,
        })
    }

    /// Sign an agent-key revocation with EIP-712 (`RevokeAgentKey`), yielding
    /// the wallet headers for `DELETE /agents/{address}`.
    ///
    /// The server accepts only this wallet signature on a revoke: HMAC, session
    /// and agent credentials are refused, so a client holding just an agent key
    /// can still revoke it.
    ///
    /// `agent` is the agent address to revoke (`0x`-prefixed, 20 bytes).
    /// `nonce` is caller-supplied Unix milliseconds: the server accepts it only
    /// within `[now - 5min, now + 60s]`, and only when it is strictly greater
    /// than the last nonce this wallet used for a rename or revoke (single use),
    /// so the current Unix-ms time is the natural choice. `chain_id` is the
    /// EIP-712 domain chain id, exactly as for
    /// [`register_agent`](Self::register_agent).
    ///
    /// `network` salts the domain the same way it does for `register_agent`,
    /// and a [`Network::Custom`] target is refused the same way.
    pub fn revoke_agent(
        &self,
        agent: &str,
        nonce: u64,
        chain_id: u64,
        network: &Network,
    ) -> Result<AgentRevocation> {
        let salt = agent_domain_salt(network, "RevokeAgentKey")?;
        let agent_addr = parse_address(agent)?;
        let digest = revoke_agent_key_digest(&self.address, &agent_addr, nonce, chain_id, &salt);
        Ok(AgentRevocation {
            account: self.address(),
            agent: to_hex_address(&agent_addr),
            nonce,
            signature: self.sign_digest(&digest)?,
            chain_id,
        })
    }

    /// Sign a 32-byte prehash, returning a `0x`-prefixed 65-byte `r||s||v`
    /// signature with `v ∈ {27, 28}` (Ethereum convention). The signature is
    /// deterministic (RFC 6979) and low-S normalized (EIP-2).
    fn sign_digest(&self, digest: &[u8; 32]) -> Result<String> {
        sign_prehash(&self.key, digest)
    }
}

/// The agent-management domain salt for `network`, or why there is none.
///
/// `message` names the EIP-712 message type being signed, for the error.
fn agent_domain_salt(network: &Network, message: &str) -> Result<[u8; 32]> {
    network
        .signing_domain()
        .and_then(|domain| domain.salt)
        .ok_or_else(|| {
            Error::invalid_request(format!(
                "no {message} signing salt is known for network {:?}: the server \
                 binds agent-management signatures to its network name (salt = \
                 keccak256(network)), and a custom target names none. Pass \
                 Network::Mainnet, Network::Testnet or Network::Local, whichever the \
                 target server runs as. The salt only names the network; the client \
                 you send the signed request through still picks the host.",
                network.label()
            ))
        })
}

/// Sign a 32-byte prehash with the hex secp256k1 key in `key`, returning a
/// `0x`-prefixed 65-byte `r||s||v` signature with `v ∈ {27, 28}`. Deterministic
/// (RFC 6979) and low-S (EIP-2). Shared by [`EthSigner`] and the agent-key
/// request signer so both emit one signature encoding.
pub(super) fn sign_prehash(key: &SecretString, digest: &[u8; 32]) -> Result<String> {
    let key = signing_key(key)?;
    let (sig, recid): (Signature, RecoveryId) = key
        .sign_prehash_recoverable(digest)
        .map_err(|_| Error::credentials("failed to sign digest"))?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = 27 + recid.to_byte();
    Ok(format!("0x{}", hex::encode(out)))
}

/// Decode the hex private key into a [`SigningKey`], with the intermediate
/// bytes zeroized on drop. The `SigningKey` itself zeroizes its scalar on drop.
pub(super) fn signing_key(key: &SecretString) -> Result<SigningKey> {
    let stripped = strip_0x(key.expose_secret());
    let bytes = Zeroizing::new(
        hex::decode(stripped).map_err(|_| Error::credentials("private key must be hex"))?,
    );
    if bytes.len() != 32 {
        return Err(Error::credentials("private key must be 32 bytes"));
    }
    SigningKey::from_slice(&bytes).map_err(|_| Error::credentials("invalid secp256k1 private key"))
}

/// Derive the 20-byte Ethereum address: `keccak256(uncompressed_pubkey[1..])[12..]`.
pub(super) fn address_of(key: &SigningKey) -> [u8; 20] {
    let point = key.verifying_key().to_encoded_point(false);
    // `point` is 65 bytes: 0x04 || X(32) || Y(32). Hash the 64 coordinate bytes.
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    addr
}

/// EIP-191 `personal_sign` digest:
/// `keccak256("\x19Ethereum Signed Message:\n" || len(msg) || msg)`.
fn eip191_digest(message: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(b"\x19Ethereum Signed Message:\n");
    hasher.update(message.len().to_string().as_bytes());
    hasher.update(message);
    finalize32(hasher)
}

/// The agent-management EIP-712 domain separator: the `Nexus Exchange` domain
/// with `chain_id`, `salt` and no `verifyingContract`. Shared by
/// `RegisterAgent` and `RevokeAgentKey`.
fn domain_separator(chain_id: u64, salt: &[u8; 32]) -> [u8; 32] {
    let domain_type_hash =
        Keccak256::digest(b"EIP712Domain(string name,string version,uint256 chainId,bytes32 salt)");
    let mut dh = Keccak256::new();
    dh.update(domain_type_hash);
    dh.update(Keccak256::digest(EIP712_DOMAIN_NAME.as_bytes()));
    dh.update(Keccak256::digest(EIP712_DOMAIN_VERSION.as_bytes()));
    dh.update(u256(chain_id));
    dh.update(salt);
    finalize32(dh)
}

/// `keccak256(0x1901 || domainSeparator || hashStruct)` under the
/// agent-management domain.
fn typed_data_digest(chain_id: u64, salt: &[u8; 32], hash_struct: Keccak256) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update([0x19, 0x01]);
    h.update(domain_separator(chain_id, salt));
    h.update(hash_struct.finalize());
    finalize32(h)
}

/// EIP-712 digest for `RegisterAgent{agent, expiresAt, nonce}`. Matches the
/// server's `agent_store::eip712::register_agent_digest`.
fn register_agent_digest(
    agent: &[u8; 20],
    expires_at: u64,
    nonce: u64,
    chain_id: u64,
    salt: &[u8; 32],
) -> [u8; 32] {
    let struct_type_hash =
        Keccak256::digest(b"RegisterAgent(address agent,uint64 expiresAt,uint64 nonce)");
    let mut sh = Keccak256::new();
    sh.update(struct_type_hash);
    sh.update(address_word(agent));
    sh.update(u256(expires_at));
    sh.update(u256(nonce));
    typed_data_digest(chain_id, salt, sh)
}

/// EIP-712 digest for `RevokeAgentKey{account, agent, nonce}`. Matches the
/// accounts service's `agent_management_auth` revoke digest.
fn revoke_agent_key_digest(
    account: &[u8; 20],
    agent: &[u8; 20],
    nonce: u64,
    chain_id: u64,
    salt: &[u8; 32],
) -> [u8; 32] {
    let struct_type_hash =
        Keccak256::digest(b"RevokeAgentKey(address account,address agent,uint64 nonce)");
    let mut sh = Keccak256::new();
    sh.update(struct_type_hash);
    sh.update(address_word(account));
    sh.update(address_word(agent));
    sh.update(u256(nonce));
    typed_data_digest(chain_id, salt, sh)
}

/// Collect a Keccak256 hasher into a fixed `[u8; 32]`.
pub(super) fn finalize32(hasher: Keccak256) -> [u8; 32] {
    let out = hasher.finalize();
    let mut d = [0u8; 32];
    d.copy_from_slice(&out);
    d
}

/// Left-pad a `u64` into a 32-byte big-endian ABI word (`uint256`).
pub(super) fn u256(v: u64) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[24..].copy_from_slice(&v.to_be_bytes());
    b
}

/// Right-align a 20-byte address into a 32-byte ABI word (`address`).
pub(super) fn address_word(addr: &[u8; 20]) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[12..].copy_from_slice(addr);
    b
}

/// Strip a `0x`/`0X` prefix if present.
fn strip_0x(s: &str) -> &str {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
}

/// Parse a `0x`-prefixed 20-byte hex address.
pub(super) fn parse_address(s: &str) -> Result<[u8; 20]> {
    let bytes = hex::decode(strip_0x(s))
        .map_err(|_| Error::invalid_request("agent address must be hex"))?;
    if bytes.len() != 20 {
        return Err(Error::invalid_request("agent address must be 20 bytes"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&bytes);
    Ok(a)
}

/// Render a 20-byte address as lowercase `0x`-prefixed hex.
pub(super) fn to_hex_address(addr: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::VerifyingKey;

    // Canonical Hardhat/ethers account #0: this private key derives to this
    // address. Pins the keccak + public-key-to-address derivation against a
    // widely published, externally verifiable vector.
    const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const TEST_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

    #[test]
    fn derives_known_address() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        assert_eq!(signer.address(), TEST_ADDR);
    }

    #[test]
    fn from_hex_accepts_0x_prefix() {
        let signer = EthSigner::from_hex(format!("0x{TEST_KEY}")).unwrap();
        assert_eq!(signer.address(), TEST_ADDR);
    }

    #[test]
    fn rejects_bad_key() {
        assert!(matches!(
            EthSigner::from_hex("zz"),
            Err(Error::Terminal(crate::TerminalError::Credentials(_)))
        ));
        assert!(matches!(
            EthSigner::from_hex("00"),
            Err(Error::Terminal(crate::TerminalError::Credentials(_)))
        ));
    }

    #[test]
    fn sign_in_recovers_to_signer() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let req = signer.sign_in().unwrap();
        assert_eq!(req.message, SIGN_IN_MESSAGE);
        let digest = eip191_digest(SIGN_IN_MESSAGE.as_bytes());
        assert_eq!(
            address_from_signature(&req.signature, &digest),
            parse_address(TEST_ADDR).unwrap()
        );
    }

    #[test]
    fn register_agent_recovers_to_wallet() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let req = signer
            .register_agent(
                KAT_AGENT,
                KAT_EXPIRES_MS,
                KAT_NONCE,
                KAT_CHAIN_ID,
                &Network::Testnet,
                Some("my-bot".into()),
            )
            .unwrap();
        assert_eq!(req.wallet, TEST_ADDR);
        assert_eq!(req.agent, KAT_AGENT);
        assert_eq!(req.expires_at, KAT_EXPIRES_MS);
        assert_eq!(req.nonce, KAT_NONCE);
        assert_eq!(req.label.as_deref(), Some("my-bot"));

        let digest = register_agent_digest(
            &parse_address(KAT_AGENT).unwrap(),
            KAT_EXPIRES_MS,
            KAT_NONCE,
            KAT_CHAIN_ID,
            &network_salt("testnet"),
        );
        assert_eq!(
            address_from_signature(&req.signature, &digest),
            parse_address(TEST_ADDR).unwrap()
        );
    }

    // `sign_in` is pinned against an independent EIP-191 implementation (ethers
    // v6) over `TEST_KEY`. `RegisterAgent` is pinned against the server itself:
    // the inputs and digest below are `agent_store::tests::
    // eip712_register_agent_digest_pinned` verbatim (alloy, `Network::Testnet`
    // salt since ENG-15643). A wrong-but-self-consistent domain separator, type
    // string, salt, or field order is caught here, unlike the recover→address
    // round-trips, which only prove internal consistency. The signature over
    // that digest is pinned identically in the Python and TypeScript SDKs.
    const KAT_AGENT: &str = "0xaaaaaaaaaaaaaaaaaaaabbbbbbbbbbbbbbbbbbbb";
    const KAT_EXPIRES_MS: u64 = 1_700_000_000;
    const KAT_NONCE: u64 = 1;
    const KAT_CHAIN_ID: u64 = 20056;

    #[test]
    fn sign_in_matches_known_answer() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let digest = eip191_digest(SIGN_IN_MESSAGE.as_bytes());
        assert_eq!(
            format!("0x{}", hex::encode(digest)),
            "0x99efa412eaa32f8d4ad2be2cad8835efc063776eff7834ddd3a8e34da9cd6268"
        );
        assert_eq!(
            signer.sign_in().unwrap().signature,
            "0xff4ddf3b1af438fe00d02368ad8fa5fc5e57667e6826dbda3ddddc395a5287bb6eab0bc97652f6e7e1f08f665b868ca143da79e18dae8021799cdafc4af670ea1b"
        );
    }

    #[test]
    fn register_agent_matches_the_servers_salted_vector() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let agent_addr = parse_address(KAT_AGENT).unwrap();
        let digest = register_agent_digest(
            &agent_addr,
            KAT_EXPIRES_MS,
            KAT_NONCE,
            KAT_CHAIN_ID,
            &network_salt("testnet"),
        );
        assert_eq!(
            hex::encode(digest),
            "5a52159bdde9c9ba6c1880598078c3326e8e32ea39c93425baafc76590d2a902"
        );
        let req = signer
            .register_agent(
                KAT_AGENT,
                KAT_EXPIRES_MS,
                KAT_NONCE,
                KAT_CHAIN_ID,
                &Network::Testnet,
                None,
            )
            .unwrap();
        assert_eq!(
            req.signature,
            "0x40cc533ba443982d33463c30426a3e81569d07d68be841daefb2bf6baf4c890403efb48f19c76ab06bceec7530b149a5c91d71688f6c7009de47a99d2e68af951c"
        );
    }

    /// Pinned against the accounts service itself: inputs and digest are its
    /// `PINNED_REVOKE` (testnet salt), the same pin mm-runner checks.
    #[test]
    fn revoke_agent_matches_the_accounts_service_pin() {
        let digest = revoke_agent_key_digest(
            &[0x11; 20],
            &[0xab; 20],
            1_790_000_000_000,
            20_056,
            &network_salt("testnet"),
        );
        assert_eq!(
            hex::encode(digest),
            "73669adde69e6f7cd9f9ecc0825887403ee05d5fc6322d462e91c42920192bcb"
        );
    }

    #[test]
    fn revoke_agent_recovers_to_wallet() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let agent = "0xABABABABABABABABABABABABABABABABABABABAB";
        let rev = signer
            .revoke_agent(agent, 1_790_000_000_000, KAT_CHAIN_ID, &Network::Testnet)
            .unwrap();
        assert_eq!(rev.account, TEST_ADDR);
        assert_eq!(rev.agent, agent.to_lowercase());
        assert_eq!(rev.nonce, 1_790_000_000_000);
        assert_eq!(rev.chain_id, KAT_CHAIN_ID);

        let digest = revoke_agent_key_digest(
            &parse_address(TEST_ADDR).unwrap(),
            &[0xab; 20],
            1_790_000_000_000,
            KAT_CHAIN_ID,
            &network_salt("testnet"),
        );
        assert_eq!(
            address_from_signature(&rev.signature, &digest),
            parse_address(TEST_ADDR).unwrap()
        );
    }

    /// The salts published in the spec's `x-nexus-networks[*].signing_domain`.
    #[test]
    fn network_salt_matches_the_spec() {
        for (network, want) in [
            (
                Network::Testnet,
                "d992b760ba3914309086be769796784454b6684e49ebfe3005bb9455433b7c8e",
            ),
            (
                Network::Mainnet,
                "7beafa94c8bfb8f1c1a43104a34f72c524268aafbfe83bff17485539345c66ff",
            ),
            (
                Network::Local,
                "98591f89798185a27bc859ebabeeae88a1ed96bfbdf2f01b32ac97474b024894",
            ),
        ] {
            let salt = network.signing_domain().and_then(|d| d.salt).unwrap();
            assert_eq!(hex::encode(salt), want, "{network:?}");
        }
    }

    #[test]
    fn register_agent_is_network_scoped() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let sigs: std::collections::HashSet<String> =
            [Network::Mainnet, Network::Testnet, Network::Local]
                .iter()
                .map(|n| {
                    signer
                        .register_agent(KAT_AGENT, KAT_EXPIRES_MS, KAT_NONCE, KAT_CHAIN_ID, n, None)
                        .unwrap()
                        .signature
                })
                .collect();
        assert_eq!(sigs.len(), 3);
    }

    /// A custom target names no network, so there is no salt to sign under,
    /// and an unsalted registration or revocation would only be refused by the
    /// server.
    #[test]
    fn agent_signing_refuses_a_target_with_no_salt() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let custom = crate::CustomNetwork::new("dev", "http://localhost:1", crate::Funds::Play)
            .unwrap()
            .with_signing_domain(crate::SigningDomain::new(KAT_CHAIN_ID));
        let network = Network::Custom(custom);
        assert_eq!(network.signing_domain().unwrap().salt, None);
        assert!(matches!(
            signer.register_agent(KAT_AGENT, KAT_EXPIRES_MS, KAT_NONCE, KAT_CHAIN_ID, &network, None),
            Err(Error::Terminal(crate::TerminalError::InvalidRequest(msg)))
                if msg.contains("no RegisterAgent signing salt")
        ));
        assert!(matches!(
            signer.revoke_agent(KAT_AGENT, KAT_NONCE, KAT_CHAIN_ID, &network),
            Err(Error::Terminal(crate::TerminalError::InvalidRequest(msg)))
                if msg.contains("no RevokeAgentKey signing salt")
        ));
    }

    #[test]
    fn agent_signing_rejects_bad_agent_address() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        assert!(matches!(
            signer.register_agent("0x1234", 1, 1, 1, &Network::Testnet, None),
            Err(Error::Terminal(crate::TerminalError::InvalidRequest(_)))
        ));
        assert!(matches!(
            signer.revoke_agent("0x1234", 1, 1, &Network::Testnet),
            Err(Error::Terminal(crate::TerminalError::InvalidRequest(_)))
        ));
    }

    #[test]
    fn label_omitted_when_none() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let req = signer
            .register_agent(
                KAT_AGENT,
                KAT_EXPIRES_MS,
                KAT_NONCE,
                KAT_CHAIN_ID,
                &Network::Testnet,
                None,
            )
            .unwrap();
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("label"));
    }

    // --- recovery helper (test-only) ------------------------------------

    /// Recover the signer's address from a 65-byte `0x` signature over `digest`,
    /// validating both the recovery id (`v`) and the digest are correct.
    fn address_from_signature(sig_hex: &str, digest: &[u8; 32]) -> [u8; 20] {
        let raw = hex::decode(strip_0x(sig_hex)).unwrap();
        assert_eq!(raw.len(), 65, "signature must be 65 bytes");
        let sig = Signature::from_slice(&raw[..64]).unwrap();
        let recid = RecoveryId::from_byte(raw[64] - 27).unwrap();
        let vk = VerifyingKey::recover_from_prehash(digest, &sig, recid).unwrap();
        let point = vk.to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&hash[12..]);
        addr
    }
}
