# nexus-exchange

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](#license)

Official Rust SDK for the [Nexus Exchange](https://exchange.nexus.xyz) API — a
thin, idiomatic wrapper over the public REST + WebSocket API.

> **Status: early.** This is the crate skeleton; endpoints land incrementally.

## Design

- Thin wrapper — typed methods that mirror the API routes, request signing, and
  serde models. Minimal business logic.
- `reqwest` + `tokio`; WebSocket via `tokio-tungstenite`.
- Money as `rust_decimal::Decimal`; one `thiserror` error type.
- Rate-limit aware — honors `429` + `Retry-After`, and an optional cost-weighted
  token bucket paces requests proactively. The bucket self-tunes to the caller's
  real tier via `429` headers and `Client::fetch_rate_limit_status`. Configure or
  disable it through `Config::with_rate_limit` / `Config::without_rate_limiter`.

## Examples

Runnable, copy-pasteable programs live under [`examples/`](./examples) and
double as the primary docs. Run one with `cargo run --example <name>`:

| Example | Auth | What it shows |
|---|---|---|
| `public_endpoints` | no | Markets, tickers, top of book |
| `orderbook_snapshot` | no | Full order-book snapshot + spread |
| `recent_trades` | no | Recent public trade prints |
| `place_order` | yes | Normalize to tick/lot, then place a limit order |
| `cancel_order` | yes | Cancel one order by id, one market, or cancel all |
| `account_balances` | yes | Balance, collateral, equity, margin |
| `positions` | yes | Open positions with PnL and liquidation price |

The two WebSocket streaming examples (`ws_orderbook`, `ws_user_events`) land
separately via [#37](https://github.com/nexus-xyz/nexus-exchange-rs/pull/37),
which merges after this PR.

Authenticated examples read `NEXUS_API_KEY` / `NEXUS_API_SECRET` from the
environment and default to a non-production network where they mutate state.

## API version

<!-- api-version-sync:start -->

Currently targets Exchange API spec **`v0.6.2`**.

<!-- api-version-sync:end -->

The pinned version lives in [`.api-version`](./.api-version); the spec itself is
published by
[`nexus-xyz/nexus-exchange-api`](https://github.com/nexus-xyz/nexus-exchange-api).
This repo does not vendor a copy — `spec-drift` CI fetches the pinned release to
check for drift, and the scheduled `api-version-sync` workflow opens a PR when a
newer spec releases. The line above is bot-managed; the table below is
maintained by hand when an SDK release ships a new pin.

| SDK version | API spec |
|---|---|
| `0.3.x` | `v0.4.0` |
| `0.1.x`–`0.2.x` | `v0.3.5` |

## License

Dual-licensed under [MIT](./LICENSE-MIT) or [Apache-2.0](./LICENSE-APACHE), at
your option.
