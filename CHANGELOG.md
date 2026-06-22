# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/nexus-xyz/nexus-exchange-rs/releases/tag/v0.1.0) - 2026-06-22

### Added

- per-request timeout + transient-only retry layer ([#21](https://github.com/nexus-xyz/nexus-exchange-rs/pull/21))
- Tier 3 — leverage/margin, order amend, batch cancel, client order ids, sub-accounts ([#28](https://github.com/nexus-xyz/nexus-exchange-rs/pull/28))
- WS reconnect with exponential backoff + jitter and bounded channels ([#22](https://github.com/nexus-xyz/nexus-exchange-rs/pull/22))
- honor 429 + Retry-After and add cost-weighted client-side rate limiter ([#20](https://github.com/nexus-xyz/nexus-exchange-rs/pull/20))
- tick/lot rounding + order limit validation helpers ([#19](https://github.com/nexus-xyz/nexus-exchange-rs/pull/19))
- SDK core — public market data, auth, account, orders, deposits, examples ([#16](https://github.com/nexus-xyz/nexus-exchange-rs/pull/16))
- add release-plz SDK release automation (ENG-3385) ([#24](https://github.com/nexus-xyz/nexus-exchange-rs/pull/24))
- core request client + public market-data endpoints (markets, ticker, health) ([#2](https://github.com/nexus-xyz/nexus-exchange-rs/pull/2))

### Other

- spec-drift check against the pinned spec ([#10](https://github.com/nexus-xyz/nexus-exchange-rs/pull/10))
- reviewer hand-off governance (CODEOWNERS, templates, CI checks) ([#13](https://github.com/nexus-xyz/nexus-exchange-rs/pull/13))
- Tier 1 docs polish — Dependabot, docs.rs metadata, missing_docs ([#26](https://github.com/nexus-xyz/nexus-exchange-rs/pull/26))
- Add cursor/time auto-paging Paginator for list endpoints ([#18](https://github.com/nexus-xyz/nexus-exchange-rs/pull/18))
- add cargo-deny supply-chain checks ([#4](https://github.com/nexus-xyz/nexus-exchange-rs/pull/4))
- Bootstrap nexus-exchange crate skeleton + CI ([#1](https://github.com/nexus-xyz/nexus-exchange-rs/pull/1))
- Initial commit: README and licenses

### Added

- Initial crate skeleton: REST client, WebSocket support, auth/signing,
  typed request/response models, and error handling.
- Targets Exchange API spec `v0.3.3`.
