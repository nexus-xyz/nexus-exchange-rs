# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- *(client)* send an `X-Nexus-Api-Version` header on every request, sourced from
  the pinned `.api-version` spec tag (currently `v0.6.2`) so it never drifts,
  and confirm the `User-Agent` is `nexus-exchange-rs/<crate version>`, for
  edge usage metering (ENG-4804, ENG-5954). Both headers also ride the WebSocket
  upgrade. Additive default headers only — no API change and not breaking.
- Extended the `spec-drift` CI gate to validate **enum members**, not just
  schema/endpoint and struct-field names (ENG-5474). A new invariant diffs a
  representative set of hand-written enums against the released spec,
  bidirectionally: it fails when the spec defines an enum value the SDK does not
  model (the class that let `PostOnly` time-in-force, ENG-5058, and the WS
  `Channel::Liquidations` variant, ENG-4646, slip through) **and** when the SDK
  models a value the spec lacks. Covers the serde enums in `src/types.rs`
  (against each spec schema property's `enum` array) and the WebSocket `Channel`
  enum in `src/ws/protocol.rs` (against the channels the spec documents for
  `GET /ws`). Intentional divergence is documented via the
  `ENUM_MEMBERS_AHEAD_OF_SPEC` / `WS_CHANNELS_AHEAD_OF_SPEC` allowlists (with a
  stale-entry check), mirroring `MODEL_FIELDS_AHEAD_OF_SPEC`. A stdlib
  regression test (`scripts/test_check_spec_drift.py`) runs in the same gate.
  No library API change.

## [0.5.1](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.5.0...v0.5.1) - 2026-07-08

### Added

- *(rest)* route migrated endpoints to the /api/v1 direct surface (ENG-4947) ([#85](https://github.com/nexus-xyz/nexus-exchange-rs/pull/85))
- add isolated-margin adjust method (ENG-4977) ([#84](https://github.com/nexus-xyz/nexus-exchange-rs/pull/84))

### Fixed

- *(rest)* encode market_id path segment in public market endpoints (ENG-4135) ([#87](https://github.com/nexus-xyz/nexus-exchange-rs/pull/87))

### Other

- bump dtolnay/rust-toolchain ([#82](https://github.com/nexus-xyz/nexus-exchange-rs/pull/82))
- bump actions/upload-artifact from 4 to 7 ([#71](https://github.com/nexus-xyz/nexus-exchange-rs/pull/71))
- bump actions/cache from 4 to 6 ([#70](https://github.com/nexus-xyz/nexus-exchange-rs/pull/70))
- pin cargo-semver-checks so the break/infra classifier stays accurate (ENG-4136) ([#88](https://github.com/nexus-xyz/nexus-exchange-rs/pull/88))
- *(spec)* bump .api-version to v0.6.2 ([#86](https://github.com/nexus-xyz/nexus-exchange-rs/pull/86))
- bump backon in the cargo-minor group across 1 directory ([#40](https://github.com/nexus-xyz/nexus-exchange-rs/pull/40))
- README no longer calls the SDK a skeleton; link examples + CLI ([#77](https://github.com/nexus-xyz/nexus-exchange-rs/pull/77))

### Changed

- Migrated the market-data and account/trading endpoints to the direct-service
  `/api/v1` surface (ENG-4947): they are now served at the **host root** instead
  of the `/api/exchange` gateway, matching the gateway-elimination work
  (ENG-4740). The Rust method surface is unchanged — only the wire path/base
  moves — so this is not a source-breaking change. Endpoints with no `/api/v1`
  variant yet (health, keys, agents, wallet auth, deposits/withdrawals, ADL,
  admin, WebSocket-token, `GET /orders/{id}`, and the tier-3 endpoints) stay on
  the gateway (dual-stack, ENG-4751). `Config` gains a `direct_base_url` (host
  root) alongside the gateway `base_url` — set from the `Network`, derived from
  `with_base_url` (strips a trailing `/api/exchange`), or overridden with
  `Config::with_direct_base_url`. Signed `/api/v1` requests sign the **full path
  including the prefix**, matching the server (the gateway strips its prefix
  before signing; the direct surface does not). The `/api/v1` routes landed in
  the `v0.6.2` spec (ENG-4943 / `nexus-exchange-api#41`).

## [0.5.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.4.1...v0.5.0) - 2026-07-02

### Other

- add WebSocket examples to README ([#79](https://github.com/nexus-xyz/nexus-exchange-rs/pull/79))

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
