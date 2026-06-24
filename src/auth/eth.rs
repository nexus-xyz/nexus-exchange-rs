//! EVM wallet signing for the two wallet-authorized flows: EIP-191 session
//! login (`signIn`) and EIP-712 agent-key registration (`registerAgent`).
//!
//! [`EthSigner`] holds a secp256k1 private key in a [`SecretString`] and
//! produces the *signed request bodies* for those endpoints. It is a pure
//! signer: deterministic, side-effect free, and ignorant of the network — the
//! caller hands the body to the [`Client`](crate::Client) to send. Nonces and
//! expiries are caller-supplied so signing carries no hidden clock.

use crate::{Error, Result};
use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use sha3::{Digest, Keccak256};
use zeroize::Zeroizing;

/// The exact, fixed message the API requires for EIP-191 session login.
pub const SIGN_IN_MESSAGE: &str = "Sign in to Nexus Exchange";

/// EIP-712 domain `name`, per the `/agents/register` spec.
const EIP712_DOMAIN_NAME: &str = "Nexus Exchange";
/// EIP-712 domain `version`, per the `/agents/register` spec.
const EIP712_DOMAIN_VERSION: &str = "1";

/// Signed body for `POST /auth/login` (EIP-191 session login).
///
/// Produced by [`EthSigner::sign_in`]; hand it to
/// [`Client::sign_in`](crate::Client::sign_in).
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
    pub fn register_agent(
        &self,
        agent: &str,
        expires_at_ms: u64,
        nonce: u64,
        chain_id: u64,
        label: Option<String>,
    ) -> Result<AgentRegistration> {
        let agent_addr = parse_address(agent)?;
        let digest = register_agent_digest(&agent_addr, expires_at_ms, nonce, chain_id);
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

    /// Sign a 32-byte prehash, returning a `0x`-prefixed 65-byte `r||s||v`
    /// signature with `v ∈ {27, 28}` (Ethereum convention). The signature is
    /// deterministic (RFC 6979) and low-S normalized (EIP-2).
    fn sign_digest(&self, digest: &[u8; 32]) -> Result<String> {
        let key = signing_key(&self.key)?;
        let (sig, recid): (Signature, RecoveryId) = key
            .sign_prehash_recoverable(digest)
            .map_err(|_| Error::credentials("failed to sign digest"))?;
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&sig.to_bytes());
        out[64] = 27 + recid.to_byte();
        Ok(format!("0x{}", hex::encode(out)))
    }
}

/// Decode the hex private key into a [`SigningKey`], with the intermediate
/// bytes zeroized on drop. The `SigningKey` itself zeroizes its scalar on drop.
fn signing_key(key: &SecretString) -> Result<SigningKey> {
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
fn address_of(key: &SigningKey) -> [u8; 20] {
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

/// EIP-712 digest for `RegisterAgent{agent, expiresAt, nonce}` under the
/// `Nexus Exchange` domain (no `verifyingContract`):
/// `keccak256(0x1901 || domainSeparator || hashStruct(message))`.
fn register_agent_digest(agent: &[u8; 20], expires_at: u64, nonce: u64, chain_id: u64) -> [u8; 32] {
    let domain_type_hash =
        Keccak256::digest(b"EIP712Domain(string name,string version,uint256 chainId)");
    let mut dh = Keccak256::new();
    dh.update(domain_type_hash);
    dh.update(Keccak256::digest(EIP712_DOMAIN_NAME.as_bytes()));
    dh.update(Keccak256::digest(EIP712_DOMAIN_VERSION.as_bytes()));
    dh.update(u256(chain_id));
    let domain_separator = dh.finalize();

    let struct_type_hash =
        Keccak256::digest(b"RegisterAgent(address agent,uint64 expiresAt,uint64 nonce)");
    let mut sh = Keccak256::new();
    sh.update(struct_type_hash);
    sh.update(address_word(agent));
    sh.update(u256(expires_at));
    sh.update(u256(nonce));
    let hash_struct = sh.finalize();

    let mut h = Keccak256::new();
    h.update([0x19, 0x01]);
    h.update(domain_separator);
    h.update(hash_struct);
    finalize32(h)
}

/// Collect a Keccak256 hasher into a fixed `[u8; 32]`.
fn finalize32(hasher: Keccak256) -> [u8; 32] {
    let out = hasher.finalize();
    let mut d = [0u8; 32];
    d.copy_from_slice(&out);
    d
}

/// Left-pad a `u64` into a 32-byte big-endian ABI word (`uint256`).
fn u256(v: u64) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[24..].copy_from_slice(&v.to_be_bytes());
    b
}

/// Right-align a 20-byte address into a 32-byte ABI word (`address`).
fn address_word(addr: &[u8; 20]) -> [u8; 32] {
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
fn parse_address(s: &str) -> Result<[u8; 20]> {
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
fn to_hex_address(addr: &[u8; 20]) -> String {
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
        let agent = "0x1234567890abcdef1234567890abcdef12345678";
        let req = signer
            .register_agent(agent, 1_782_000_000_000, 1, 393, Some("my-bot".into()))
            .unwrap();
        assert_eq!(req.wallet, TEST_ADDR);
        assert_eq!(req.agent, agent);
        assert_eq!(req.expires_at, 1_782_000_000_000);
        assert_eq!(req.nonce, 1);
        assert_eq!(req.label.as_deref(), Some("my-bot"));

        let digest =
            register_agent_digest(&parse_address(agent).unwrap(), 1_782_000_000_000, 1, 393);
        assert_eq!(
            address_from_signature(&req.signature, &digest),
            parse_address(TEST_ADDR).unwrap()
        );
    }

    // Known-answer vectors produced by an independent EIP-712/EIP-191
    // implementation (ethers v6) over `TEST_KEY`. These pin the exact digests
    // and 65-byte signatures, so a wrong-but-self-consistent domain separator,
    // type string, or field order is caught here — unlike the recover→address
    // round-trips, which only prove internal consistency. Regenerating them
    // requires matching the server's typed data verbatim
    // (`name: "Nexus Exchange"`, `RegisterAgent(address agent,uint64 expiresAt,uint64 nonce)`).
    const KAT_AGENT: &str = "0x1234567890abcdef1234567890abcdef12345678";
    const KAT_EXPIRES_MS: u64 = 1_782_000_000_000;
    const KAT_NONCE: u64 = 1;
    const KAT_CHAIN_ID: u64 = 393;

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
    fn register_agent_matches_known_answer() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let agent_addr = parse_address(KAT_AGENT).unwrap();
        let digest = register_agent_digest(&agent_addr, KAT_EXPIRES_MS, KAT_NONCE, KAT_CHAIN_ID);
        assert_eq!(
            format!("0x{}", hex::encode(digest)),
            "0x356e6f3d741f48279c78b228d4ed9217eb49ad9179d549c618215be57817bfd6"
        );
        let req = signer
            .register_agent(KAT_AGENT, KAT_EXPIRES_MS, KAT_NONCE, KAT_CHAIN_ID, None)
            .unwrap();
        assert_eq!(
            req.signature,
            "0x5df263ed6d1b619a72d436a01104f9036af6258cacf56dea973321cbe722a99550644eea6bf75656d48e982d2ce5db9ef13c4aced4539cf3c2ff87802b0197cc1b"
        );
    }

    #[test]
    fn register_agent_rejects_bad_agent_address() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        assert!(matches!(
            signer.register_agent("0x1234", 1, 1, 1, None),
            Err(Error::Terminal(crate::TerminalError::InvalidRequest(_)))
        ));
    }

    #[test]
    fn label_omitted_when_none() {
        let signer = EthSigner::from_hex(TEST_KEY).unwrap();
        let req = signer
            .register_agent(
                "0x1234567890abcdef1234567890abcdef12345678",
                1_782_000_000_000,
                1,
                393,
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
