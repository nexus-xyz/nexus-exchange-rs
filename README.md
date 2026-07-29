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

## Pagination

The cursor-paginated list endpoints return one page plus an opaque cursor for the
next, carried in the **`X-Next-Cursor`** response header (spec v0.7.2). The
`*_paginated` methods hand back a `Paginator` that drives that cursor for you —
nothing is requested until a page is asked for:

```rust
// Everything, in one Vec.
let fills = client.fetch_my_trades_paginated().page_size(1000).all().await?;

// Or a lazy stream of items, pages fetched on demand.
let mut stream = Box::pin(client.fetch_trades_paginated("BTC-USDX-PERP")?.into_stream());
while let Some(trade) = stream.next().await { let _ = trade?; }

// Or page-by-page, persisting the cursor to resume later.
let mut pager = client.fetch_my_trades_paginated().starting_after("saved-cursor");
while let Some(page) = pager.next_page().await? {
    save_checkpoint(page.next_cursor.as_ref()); // `None` on the last page
}
```

All five cursor-paginated endpoints in spec v0.7.2 are covered, each with a flat
first-page method and a `Paginator`:

| Endpoint | First page | Whole history | `limit` max |
|---|---|---|---|
| `GET /api/v1/markets/{id}/trades` | `fetch_trades` | `fetch_trades_paginated` | `MAX_TRADES_LIMIT` = **1000** |
| `GET /api/v1/fills` | `fetch_my_trades` | `fetch_my_trades_paginated` | `MAX_FILLS_LIMIT` = **1000** |
| `GET /api/v1/orders/history` | `fetch_order_history` | `fetch_order_history_paginated` | `MAX_ORDER_HISTORY_LIMIT` = **500** |
| `GET /api/v1/positions/closed` | `fetch_closed_positions` | `fetch_closed_positions_paginated` | `MAX_CLOSED_POSITIONS_LIMIT` = **200** |
| `GET /api/v1/account/equity-history` | `fetch_equity_history` | `fetch_equity_history_paginated` | `MAX_EQUITY_HISTORY_LIMIT` = **720** |

The flat methods return the first page only, and never a cursor. Their `limit`
is the same per-endpoint bound as `page_size` below; `None` sends no `limit` at
all, leaving each endpoint's own server-side default in force (100, except
`/account/equity-history`, which defaults to its 720 maximum).

Cursors are opaque — never parse one. Termination:

- **No `X-Next-Cursor` ⇒ the last page.** Not an error, and not a reason to retry.
- An **empty page that still carries a cursor is not the end** — paging continues,
  so a sparse window does not truncate the walk.
- A server that hands back the **same** cursor it was given cannot advance, so the
  paginator returns that page and stops rather than re-issuing the identical
  request forever. (`nexus-exchange-py` raises `PaginationError` on this instead;
  here the stall is visible as a non-`None` `next_cursor` on the last page.)
- Nothing else bounds how far back a walk goes; pass `max_pages` when that matters.

`page_size` sets the per-page `limit` and is checked against **that endpoint's**
spec maximum (the table above) before the request is sent — and on the signed
routes, before it is signed. The maxima are per endpoint and **not
interchangeable**: `page_size(500)` is valid on `/orders/history` and out of range
on `/positions/closed`. Two things follow that are easy to get wrong:

- the `366` of `MAX_PORTFOLIO_HISTORY_LIMIT` belongs to
  `/account/portfolio-history`, which is not cursor-paginated at all — a shared
  clamp there would sit *below* equity-history's own default of 720 and reject a
  plain default request client-side;
- `/account/equity-history` defaults to **720**, not 100, so omitting `limit`
  there asks for the whole ~1h / 5s window and the first page is usually the last.

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
| [`portfolio`](./examples/portfolio.rs) | yes | Account state, per-position risk, fees, equity/PnL/volume history |
| [`ws_user_events`](./examples/ws_user_events.rs) | yes | Stream private per-account events (fills, orders) |

Authenticated examples read `NEXUS_API_KEY` / `NEXUS_API_SECRET` from the
environment and default to a non-production network where they mutate state.

For a complete command-line application built on the SDK — every request goes
through the crate's `Client`, with no transport of its own — see
[`nexus-exchange-cli`](https://github.com/nexus-xyz/nexus-exchange-cli).

## API version

<!-- api-version-sync:start -->

Currently targets Exchange API spec **`v0.7.2`**.

<!-- api-version-sync:end -->

The pinned version lives in [`.api-version`](./.api-version); the spec itself is
published by
[`nexus-xyz/nexus-exchange-api`](https://github.com/nexus-xyz/nexus-exchange-api).
This repo does not vendor a copy — `spec-drift` CI fetches the pinned release to
check for drift, and `spec-autobump` opens a PR when a newer spec releases
(dispatched on api-repo release, with a daily poll fallback). It classifies the
change with oasdiff: non-breaking bumps arm auto-merge, breaking ones route to a
human (ENG-3563). Only the line above is bot-managed — it tracks the pin on
`main`, including spec versions no release has shipped yet.

The table below records **released** SDK versions and the spec each one actually
shipped against, so it is history and no automation rewrites it. A new row is
appended when a release goes out. Every row is derived from the tags themselves
and can be re-checked in one command:

```sh
for t in $(git tag -l 'v*' | sort -V); do echo "$t -> $(git show "$t:.api-version")"; done
```

| SDK version | API spec |
|---|---|
| `0.6.x` | `v0.7.1` |
| `0.5.1` | `v0.6.2` |
| `0.5.0` | `v0.6.0` |
| `0.3.x`–`0.4.x` | `v0.5.0` |
| `0.1.x`–`0.2.x` | `v0.3.5` |

## License

Dual-licensed under [MIT](./LICENSE-MIT) or [Apache-2.0](./LICENSE-APACHE), at
your option.
