# nexus-exchange

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

## API version

This SDK targets a specific released version of the Exchange API spec, pinned in
[`.api-version`](./.api-version). The spec itself lives in
[`nexus-xyz/nexus-exchange-api`](https://github.com/nexus-xyz/nexus-exchange-api);
this repo does not vendor a copy — CI fetches the pinned release to check for
drift.

| SDK version | API spec |
|---|---|
| _unreleased_ | `v0.3.3` |

## License

Dual-licensed under [MIT](./LICENSE-MIT) or [Apache-2.0](./LICENSE-APACHE), at
your option.
