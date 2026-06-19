# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial crate skeleton: REST client, WebSocket support, auth/signing,
  typed request/response models, and error handling.
- Wallet-signed auth, mirroring the API: EIP-191 session login (`Client::sign_in`)
  and EIP-712 agent-key registration (`Client::register_agent`), via a thin
  `EthSigner` (secp256k1 key held in `SecretString`). Credentials now sit behind
  a `Credential` trait with a pluggable `Nonce` source (`Config::with_credential`
  / `Config::with_nonce`).
- Targets Exchange API spec `v0.3.3`.
