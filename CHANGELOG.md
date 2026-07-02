# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.4.0...v0.4.1) - 2026-07-02

### Other

- Merge pull request #33 from nexus-xyz/dependabot/github_actions/actions/checkout-7

## [0.4.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.3.0...v0.4.0) - 2026-07-02

### Added

- [**breaking**] send market_id on the by-id order routes; amend via PATCH

### Other

- make releases one-click — release-plz token fallback + advisory drift (ENG-4360)

## [0.3.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.2.0...v0.3.0) - 2026-06-26

### Added

- *(ci)* oasdiff-gated spec auto-bump pipeline (ENG-3563) ([#59](https://github.com/nexus-xyz/nexus-exchange-rs/pull/59))
- *(account)* add network-aware fund() funding convenience (ENG-4200) ([#63](https://github.com/nexus-xyz/nexus-exchange-rs/pull/63))
- *(rest)* typed Vec<OrderResult> for batch create_orders (ENG-4199) ([#62](https://github.com/nexus-xyz/nexus-exchange-rs/pull/62))
- *(orders)* add market-scoped cancel (ENG-4198) ([#61](https://github.com/nexus-xyz/nexus-exchange-rs/pull/61))
- *(rest)* login + key create/revoke + agent mgmt + HMAC ADL reads ([#38](https://github.com/nexus-xyz/nexus-exchange-rs/pull/38))
- split Error into terminal vs transient trees (ENG-3424) ([#14](https://github.com/nexus-xyz/nexus-exchange-rs/pull/14))
- auto-sync pinned API spec version with exchange-api releases ([#54](https://github.com/nexus-xyz/nexus-exchange-rs/pull/54))
- *(markets)* [**breaking**] rename MarketSummary.mark_price to last_trade_price; pin spec v0.4.0 ([#48](https://github.com/nexus-xyz/nexus-exchange-rs/pull/48))
- wallet-signed auth — EIP-191 signIn + EIP-712 registerAgent ([#36](https://github.com/nexus-xyz/nexus-exchange-rs/pull/36))
- *(ws)* typed op-envelope streaming client with cursor resume ([#44](https://github.com/nexus-xyz/nexus-exchange-rs/pull/44))
- send descriptive User-Agent for per-client traffic attribution ([#43](https://github.com/nexus-xyz/nexus-exchange-rs/pull/43))
- *(rest)* typed public market-data endpoints (ENG-3380) ([#23](https://github.com/nexus-xyz/nexus-exchange-rs/pull/23))

### Fixed

- encode address path segment in fetch_account_adl_history ([#57](https://github.com/nexus-xyz/nexus-exchange-rs/pull/57))

### Other

- bump SDK .api-version to v0.5.0 (ENG-4344) ([#67](https://github.com/nexus-xyz/nexus-exchange-rs/pull/67))
- clear stale [Unreleased] changelog so release-plz generates clean v0.3.0 (ENG-4214) ([#64](https://github.com/nexus-xyz/nexus-exchange-rs/pull/64))
- distinguish a semver tool/infra failure from a detected break ([#58](https://github.com/nexus-xyz/nexus-exchange-rs/pull/58))
- emit test-coverage % via cargo-llvm-cov (ENG-4016) ([#56](https://github.com/nexus-xyz/nexus-exchange-rs/pull/56))
- add license badge to README ([#51](https://github.com/nexus-xyz/nexus-exchange-rs/pull/51))
- add SECURITY.md pointing at private vulnerability reporting ([#53](https://github.com/nexus-xyz/nexus-exchange-rs/pull/53))
- bump hmac 0.12→0.13 and sha2 0.10→0.11 together (ENG-3899) ([#50](https://github.com/nexus-xyz/nexus-exchange-rs/pull/50))
- *(semver)* fail only on undeclared breaking API changes (ENG-3904) ([#52](https://github.com/nexus-xyz/nexus-exchange-rs/pull/52))
- route code review to @nexus-xyz/eng (+ @collinjackson) instead of a single owner ([#55](https://github.com/nexus-xyz/nexus-exchange-rs/pull/55))
- harden CI floor + add MSRV gate (ENG-3384) ([#30](https://github.com/nexus-xyz/nexus-exchange-rs/pull/30))
- harden spec-drift check: verify client code ↔ endpoints.txt ([#49](https://github.com/nexus-xyz/nexus-exchange-rs/pull/49))
- add per-PR cargo-semver-checks + compatibility/deprecation policy ([#46](https://github.com/nexus-xyz/nexus-exchange-rs/pull/46))
- *(examples)* idiomatic, copy-pasteable example programs ([#29](https://github.com/nexus-xyz/nexus-exchange-rs/pull/29))

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
