# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Market-scoped cancel: `Client::cancel_orders_for_market(market_id)` flattens a
  single market in one round-trip (`DELETE /orders?market_id=`), instead of
  `fetch_open_orders` → filter client-side → `cancel_orders`. `cancel_all_orders`
  is unchanged and still cancels account-wide. An empty `market_id` is rejected
  locally so a per-market cancel can never silently widen into a full account
  flatten (ENG-4198).
- Typed, protocol-aware streaming client: `Client::subscribe` returns a
  `MessageStream` (a `Stream` of decoded `ServerMessage`s) for the op-envelope
  protocol, covering public per-market channels (trades/book/candles) and
  per-account channels (orders/fills/positions/balances). Mints a single-use
  `/ws/token` per connection to upgrade private streams and resumes each channel
  from its `since`/`seq_at_join` cursor on reconnect.
- Wallet-signed auth, mirroring the API: EIP-191 session login (`Client::sign_in`)
  and EIP-712 agent-key registration (`Client::register_agent`), via a thin
  `EthSigner` (secp256k1 key held in `SecretString`). Credentials now sit behind
  a `Credential` trait with a pluggable `Nonce` source (`Config::with_credential`
  / `Config::with_nonce`).

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
  before signing; the direct surface does not). **Pending:** depends on the
  unreleased `/api/v1` spec (ENG-4943 / `nexus-exchange-api#41`) — the pin
  (`v0.6.1`) and the temporary `spec-drift` branch override must be finalized
  when that spec releases.
- **Breaking:** `Client::create_orders` now returns `Vec<OrderResult>` instead of
  the untyped `serde_json::Value`, so callers no longer re-serialize and
  string-parse the batch result (ENG-4199). `OrderResult` is a typed enum
  mirroring the engine's per-order outcome — `OrderResult::Placed { order, fills }`
  (same shape as the single-order `OrderResponse`) or `OrderResult::Rejected
  { error, message }` — internally tagged on the wire by `outcome` (`ok`/`err`),
  with `succeeded()` / `order()` / `error()` accessors. The batch-cancel
  `cancel_orders` is unchanged (it returns a different, cancellation-summary
  shape).

### Added

- *(ws)* connect_ws convenience + correct WS origin on Network (ENG-3398) ([#39](https://github.com/nexus-xyz/nexus-exchange-rs/pull/39))
- *(keys/agents)* add API-key create/delete and agent list/revoke ([#32](https://github.com/nexus-xyz/nexus-exchange-rs/pull/32))

### Fixed

- *(tickers)* confirm /tickers map envelope, key by market_id ([#42](https://github.com/nexus-xyz/nexus-exchange-rs/pull/42))

### Added

- `Client::connect_ws` convenience that mints a single-use token and opens the
  per-account WebSocket stream, re-minting on each reconnect.

### Changed

- **Breaking:** the WebSocket origin is no longer derived from the REST base.
  `Network::ws_url()` is now `Network::ws_base()` and `Config::ws_url()` returns
  `Option<&str>` (`None` for networks whose WS host is unconfirmed, currently
  `Stable`/`Beta`). `Config::with_base_url` no longer infers a WS URL.

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
