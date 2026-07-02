# nexus-exchange

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](#license)

Official Rust SDK for the [Nexus Exchange](https://exchange.nexus.xyz) API — a
thin, idiomatic wrapper over the public REST + WebSocket API.

> **Status: in production use.** The SDK covers the public REST + WebSocket
> surface and is what Nexus's own market-making bots trade through. The API is
> pre-1.0 and evolves with the [spec](#api-version).

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
| [`public_endpoints`](./examples/public_endpoints.rs) | no | Markets, tickers, top of book |
| [`orderbook_snapshot`](./examples/orderbook_snapshot.rs) | no | Full order-book snapshot + spread |
| [`recent_trades`](./examples/recent_trades.rs) | no | Recent public trade prints |
| [`ws_orderbook`](./examples/ws_orderbook.rs) | no | Stream live order-book updates over the WebSocket |
| [`place_order`](./examples/place_order.rs) | yes | Normalize to tick/lot, then place a limit order |
| [`cancel_order`](./examples/cancel_order.rs) | yes | Cancel one order by id, one market, or cancel all |
| [`account_balances`](./examples/account_balances.rs) | yes | Balance, collateral, equity, margin |
| [`positions`](./examples/positions.rs) | yes | Open positions with PnL and liquidation price |
| [`ws_user_events`](./examples/ws_user_events.rs) | yes | Stream private per-account events (fills, orders) |

Authenticated examples read `NEXUS_API_KEY` / `NEXUS_API_SECRET` from the
environment and default to a non-production network where they mutate state.

For a complete command-line application built on the SDK — every request goes
through the crate's `Client`, with no transport of its own — see
[`nexus-exchange-cli`](https://github.com/nexus-xyz/nexus-exchange-cli).

## API version

<!-- api-version-sync:start -->

Currently targets Exchange API spec **`v0.6.0`**.

<!-- api-version-sync:end -->

The pinned version lives in [`.api-version`](./.api-version); the spec itself is
published by
[`nexus-xyz/nexus-exchange-api`](https://github.com/nexus-xyz/nexus-exchange-api).
This repo does not vendor a copy — `spec-drift` CI fetches the pinned release to
check for drift, and `spec-autobump` opens a PR when a newer spec releases
(dispatched on api-repo release, with a daily poll fallback). It classifies the
change with oasdiff: non-breaking bumps arm auto-merge, breaking ones route to a
human (ENG-3563). The line above and the top row of the table below (the
in-development SDK series) are bot-managed; the historical rows are left as-is.

| SDK version | API spec |
|---|---|
| `0.3.x` | `v0.6.0` |
| `0.1.x`–`0.2.x` | `v0.3.5` |

## License

Dual-licensed under [MIT](./LICENSE-MIT) or [Apache-2.0](./LICENSE-APACHE), at
your option.
