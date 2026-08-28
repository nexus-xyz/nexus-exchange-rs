# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.10.0...v0.11.0) - 2026-08-28

### Added

- *(types)* [**breaking**] surface stp, max_slippage_bps and cancellation_reason (ENG-13068) ([#148](https://github.com/nexus-xyz/nexus-exchange-rs/pull/148))

### Fixed

- point the durable mainnet base at the host root, not /v1 ([#145](https://github.com/nexus-xyz/nexus-exchange-rs/pull/145))

## [0.10.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.9.1...v0.10.0) - 2026-08-24

### Added

- *(rest)* [**breaking**] delete the phantom code-only ops, seal the allowlist (ENG-8617) ([#143](https://github.com/nexus-xyz/nexus-exchange-rs/pull/143))
- *(rest)* wrap GET /markets/{market_id}/funding-samples (ENG-4159) ([#139](https://github.com/nexus-xyz/nexus-exchange-rs/pull/139))

### Fixed

- *(ci)* classify semver breaks on the summary, not the exit code (ENG-11844) ([#144](https://github.com/nexus-xyz/nexus-exchange-rs/pull/144))
- *(ci)* count spec coverage by operation, not by path spelling (ENG-11842) ([#140](https://github.com/nexus-xyz/nexus-exchange-rs/pull/140))

### Other

- bump cargo-semver-checks to 0.50.0 for rustdoc format v60 (ENG-11844) ([#141](https://github.com/nexus-xyz/nexus-exchange-rs/pull/141))

## [0.9.1](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.9.0...v0.9.1) - 2026-08-14

### Deprecated

- *(config)* **`Config::with_base_url` is deprecated in favour of
  `Network::Custom`** (ENG-10951). It is the *selector* — the argument that picks
  a target without declaring what that target moves — and a bare URL cannot say
  what a deployment is: whether its funds are real, whether it has a faucet,
  where it streams from, what domain it signs under. `Network::Custom` carries
  that bundle, so the guardrails read declared facts rather than an absence.

  ```rust
  // Before
  let config = Config::with_base_url("https://exchange.example.com/api/exchange");

  // After
  let target = CustomNetwork::new("dev", "https://exchange.example.com/api/exchange", Funds::Play)?;
  let config = Config::new(Network::Custom(target));
  ```

  **Behaviour is unchanged and nothing is removed.** `with_base_url` has been
  sugar for a `Network::Custom` with `Funds::Unknown` and no faucet, WS origin or
  signing domain since 0.9.0, so the guarded paths — `Client::fund` above all —
  already refuse on it; this release only adds the marker that says so at build
  time. The method keeps working, and removing it would be a breaking change that
  needs its own release, so downstreams are not obliged to migrate on this one.

  The *modifiers* are untouched: `Config::with_direct_base_url` and
  `Config::with_ws_url` refine a target that has already been chosen and carry no
  funds claim, so they are not deprecated.

### Other

- bump the cargo-minor group with 3 updates ([#137](https://github.com/nexus-xyz/nexus-exchange-rs/pull/137))

## [0.9.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.8.0...v0.9.0) - 2026-08-12

### Added

- *(config)* [**breaking**] add a Custom network with a caller-supplied base URL (ENG-9824) ([#133](https://github.com/nexus-xyz/nexus-exchange-rs/pull/133))
- *(ws)* add the liquidations channel and bump .api-version to v0.7.3 (ENG-7341) ([#129](https://github.com/nexus-xyz/nexus-exchange-rs/pull/129))

### Fixed

- *(config)* route /api/v1 to the gateway base, not the host root (ENG-10063) ([#131](https://github.com/nexus-xyz/nexus-exchange-rs/pull/131))

### Other

- *(changelog)* note that Custom labels reject built-in network names (ENG-9824) ([#135](https://github.com/nexus-xyz/nexus-exchange-rs/pull/135))
- bump dtolnay/rust-toolchain from 2c7215f132e9ebf062739d9130488b56d53c060c to 6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772 ([#130](https://github.com/nexus-xyz/nexus-exchange-rs/pull/130))
- bump the pinned Exchange API spec to v0.8.1 (ENG-10482) ([#134](https://github.com/nexus-xyz/nexus-exchange-rs/pull/134))
- *(spec-drift)* fail when serde_derive moves out from under the transcription ([#127](https://github.com/nexus-xyz/nexus-exchange-rs/pull/127))

### Added

- *(config)* [**breaking**] **`Network::Custom` — a caller-supplied deployment**
  (ENG-9824). Targets this crate does not name — your own environment, a preview
  host, a sandbox — still have to be reachable from it. Enumerating such hosts in
  a published client would put them in the package permanently and discoverably,
  and the list would need extending every time one was added. So the caller
  supplies the URL and `Custom` ships none. It is **client-side only**: never a
  value the server accepts, and never present in the spec's `x-nexus-networks`.

  `Custom` carries the whole safety bundle rather than just an address — a bare
  URL is what makes a client report play-funds guardrails while aimed at a
  real-funds host:

  ```rust
  let target = CustomNetwork::new("dev", "https://exchange.example.com/api/exchange", Funds::Play)?
      .with_faucet(true)
      .with_ws_url("wss://stream.example.com/ws")?;
  let client = Client::new(Config::new(Network::Custom(target)));
  ```

  New `Funds` classification — `Real`, `Play`, `Unknown` — is **required with no
  default**, because both booleans are wrong: `false` makes every guardrail lie
  in the direction that costs money, `true` makes development unusable.
  `Unknown` fails closed. The faucet, WS origin and signing domain are likewise
  absent until declared, never guessed, and the label is required because it is
  the key per-network credentials are stored under.

  A `Custom` target is reachable even when it declares `Funds::Real`, unlike
  `Network::Mainnet`. These are not in tension: `Mainnet` is refused because this
  release cannot *build* correct URLs for its durable base (the version sits in
  the base, `…/v1`, not the path), which is a URL-layout problem rather than a
  funds problem. With `Custom` the caller supplies the URL and owns the layout.
  What stays guarded is money movement — `Client::fund` claims faucet credit only
  for a declared play-funds target that declares a faucet, and refuses otherwise.

  Caller-supplied URLs are validated at construction: `http(s)` (or `ws(s)`)
  scheme only, a host, no `user:pass@` userinfo, no query or fragment, no
  whitespace or control characters. Each rejection is a URL that would otherwise
  build a *wrong* request rather than merely fail — a query swallows the appended
  path, so the request would go somewhere other than where the signature says.
  Labels are restricted to `[A-Za-z0-9._-]`, reject `.`/`..`, and reject the
  built-in network names (`mainnet`, `testnet`, `local` and the `custom` that
  `with_base_url` targets carry) case-insensitively, so a label can never address
  another target's stored credentials — neither by traversing to it nor by naming
  it. No hostname is checked against an allowlist, which is the entire point of
  the variant.

### Changed

- *(config)* [**breaking**] `Network::is_mainnet()` is **replaced by
  `Network::funds() -> Funds`**, and `Network::signing_domain()` now returns
  `Option<SigningDomain>`. Both are removals rather than deprecated aliases
  because both change *semantics*, which CONTRIBUTING calls out as the case where
  an alias preserves the old, wrong behaviour: a kept `is_mainnet()` would answer
  `false` for a `Custom` real-funds target — a guardrail failing silently in the
  money-losing direction, with only a warning to say so. Guards should match
  `Funds::Play` positively (or call `Funds::is_known_play()`) rather than negate
  the real case, so `Unknown` cannot become "safe" by default.

  Also breaking, all consequences of `Custom` carrying data: `Network` is no
  longer `Copy` (still `Clone`); `base_url()`, `direct_base_url()` and
  `ws_base()` borrow `&self` and return `&str`/`Option<&str>` rather than
  `&'static str`; and `Config::network()` returns `&Network` instead of
  `Option<Network>`. That last `Option` conflated two facts and hid the dangerous
  one — code asking *which target is this* got `None`, and code asking *does this
  move real money* had to infer its answer from the same `None`. New
  `Network::has_faucet()` and `Network::label()`; `label()` lets callers name a
  network without matching on the enum, so a variant added later cannot be
  silently mishandled.

- *(config)* `Config::with_base_url` is now sugar for a `Network::Custom` with
  `Funds::Unknown` — all a bare URL can honestly claim — so there is one
  mechanism for pointing at a host instead of two, and every guard reads the same
  fields either way. Behaviour is unchanged for existing callers: a raw base URL
  has always refused `Client::fund`, previously because the network was absent and
  now because the funds are undeclared. The URL is still not validated there,
  since the method returns `Self` and cannot report a rejection; use
  `CustomNetwork::new` for the checked path.

### Fixed

- *(config)* **`Config::direct_base_url` pointed the `/api/v1` surface at the
  host root, which serves no API** (ENG-10063). Every `/api/v1` request landed on
  the marketing frontend and came back as a `404` with an HTML body — 34 of the
  SDK's 62 targeted operations, including `POST /api/v1/orders`, both cancels,
  `/orders/batch`, `/orders/preview` and the whole authenticated account surface.

  The `/api/v1` surface is mounted **under** the `/api/exchange` gateway prefix,
  not at the host root, so the direct base and the REST base are the *same base*
  on every deployment that exists today:

  ```text
  https://exchange.nexus.xyz/api/exchange/api/v1/markets/summary  -> 200 (JSON)
  https://exchange.nexus.xyz/api/v1/markets/summary               -> 404 (frontend HTML)
  ```

  `/api/v2` and junk segments answer a JSON `NOT_FOUND` under the gateway, so the
  gateway recognizes `/api/v1` specifically — a real mount, not a permissive
  router. `Network::Testnet.direct_base_url()` is therefore now
  `https://exchange.nexus.xyz/api/exchange`, and `with_base_url` no longer strips
  a trailing `/api/exchange` to derive the direct base. This corrects the
  host-root claim in the `v0.6.0` ENG-4947 entry below, which was right that the
  full `/api/v1` path is signed but wrong about why.

  **No signing change and no path literal changes.** The client signs the
  `/api/v1/...` path literal, never base + path, and the gateway strips only its
  own prefix before the indexer verifies — which is why today's legacy signed
  calls, signing the bare `/orders`, work at all.
  `Config::with_direct_base_url` remains the override for when gateway
  elimination (ENG-4740) genuinely moves the surface.

- *(config)* `with_base_url` now trims a trailing slash from the REST base as
  well as from the derived direct base. A base passed as `…/api/exchange/` built
  `…//orders`, which is a different path than the `/orders` the client signs, so
  it failed verification rather than merely looking untidy.

## [0.8.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.7.0...v0.8.0) - 2026-08-04

### Added

- *(rest)* add fetch_account_funding for GET /funding ([#122](https://github.com/nexus-xyz/nexus-exchange-rs/pull/122))
- *(rest)* add fetch_market_risk_params for GET /markets/{market_id}/risk-params ([#121](https://github.com/nexus-xyz/nexus-exchange-rs/pull/121))
- *(rest)* add the spec'd funds surface — GET/POST /deposits and POST /faucet ([#119](https://github.com/nexus-xyz/nexus-exchange-rs/pull/119))
- *(rest)* add fetch_stats and fetch_stats_history for the venue stats reads ([#118](https://github.com/nexus-xyz/nexus-exchange-rs/pull/118))
- *(config)* [**breaking**] adopt the {Mainnet, Testnet, Local} network axis (ENG-6452) ([#126](https://github.com/nexus-xyz/nexus-exchange-rs/pull/126))

### Fixed

- *(spec-drift)* derive wire names correctly for enum variants, not just fields ([#120](https://github.com/nexus-xyz/nexus-exchange-rs/pull/120))

### Other

- *(spec-pin)* rename CI's `drift` job to `spec-pin` (ENG-7961) ([#125](https://github.com/nexus-xyz/nexus-exchange-rs/pull/125))
- *(spec-drift)* make the drift gate unskippable and unambiguous (ENG-7961) ([#123](https://github.com/nexus-xyz/nexus-exchange-rs/pull/123))
- *(contributing)* document the squash-merge conventions the PR title now drives ([#117](https://github.com/nexus-xyz/nexus-exchange-rs/pull/117))

### Changed

- *(config)* [**breaking**] Adopted the **network axis** the spec formalizes
  (ENG-6442): `Network` is now `{Mainnet, Testnet, Local}`, each bundling its
  REST bases, WebSocket origin and EIP-712 signing domain (ENG-6452).

  **The rename is a bug fix, not a relabel — read the mapping before you
  migrate.** `Stable` pointed at `https://exchange.nexus.xyz/api/exchange`, and
  the spec's authoritative `x-nexus-networks` map records that host as
  **testnet**: play funds, faucet-credited, no real-world value. So
  `Stable.is_production()` returned `true` for a play-funds host, and the
  obvious reading of the rename — `Stable → Mainnet` — would have carried that
  mislabel onto the real-funds variant and aimed `fund()`'s safety guard at the
  wrong network. The correct mapping is:

  | before | after | note |
  | -- | -- | -- |
  | `Network::Stable` | `Network::Testnet` | **not** `Mainnet` — the legacy host is play funds |
  | `Network::Beta` | `Network::Testnet`, or `Config::with_base_url` for a beta host | no beta host in the network map |
  | `Network::is_production()` | `Network::is_mainnet()` | same predicate, honest name |

  `Stable` and `Beta` are **removed outright** rather than left as deprecated
  aliases, so the compiler makes every call site re-decide which network it
  meant. A silent remap is exactly what must not happen here.

  Testnet deliberately keeps the **legacy** base. Its durable replacement
  `api.testnet.nexus.xyz` does not resolve yet and also changes the path layout
  (`/v1` in the base), so the spec says to keep pinning the legacy base until
  the hosts are live; moving is its own change.

- *(config)* [**breaking**] `Config::default()` now targets `Network::Testnet`
  (was `Stable`). Both are the same host today, so no request changes
  destination — but the default is now *named* play funds, and the default must
  never be a real-funds network.

- *(rest)* [**breaking behaviour**] **`fund()` on the default network now claims
  faucet credit instead of erroring.** This is the correct behaviour — that host
  is play funds — but it is a real change for anyone who relied on `Stable`
  erroring as a guard. `fund()` still refuses on `Mainnet` and on an unknown
  host (`Config::with_base_url`), and still rejects a non-positive amount first.
  If you were treating the old error as "don't fund here", switch to an explicit
  `deposit()` / `claim_credit()` call.

### Added

- *(config)* `Network::Mainnet`, the real-funds network — **declared but not
  targetable by this release.** A `Mainnet` client builds fine, and then refuses
  every request locally, before any DNS, TLS, byte on the wire, or use of a
  credential. Two independent reasons, either sufficient: `api.nexus.xyz` does
  not resolve yet (ENG-8155), and its durable base carries the version *in the
  base* (`…/v1`) rather than in the path, which is not the dual-stack layout
  this SDK builds and signs — sending it there would produce wrong URLs *and* a
  signature over a path the server never sees. Guessing either against a
  real-funds host is the precise failure the network axis exists to prevent, so
  the SDK fails closed and loudly. To target a mainnet host you control, use
  `Config::with_base_url`.

  The gate lives in the single base-resolution choke point every request builder
  already goes through, which now returns `Result`, so **the check is enforced by
  the compiler**: a future builder that forgets it does not compile. A boolean
  consulted per call site would be one forgotten `if` away from putting a
  wrongly-signed request on a real-money host.

- *(config)* `Network::signing_domain()` returning the new public
  `SigningDomain { name, version, chain_id }`, spelled as in the spec's
  `x-nexus-networks[*].signing_domain`. **`chain_id` is always `None`**, which
  means "this SDK does not publish the value" — *not* zero. The signing domain is
  per-network and server-authoritative; read the chain id from `/metadata` for
  the network you are connected to. A client that cannot obtain one must refuse
  to sign rather than default, because a wrong domain either fails verification
  or produces a signature valid on a *different* network. `EthSigner::register_agent`
  keeps taking `chain_id` explicitly for the same reason. `name`/`version` are
  sourced from the module that actually signs, so what the SDK advertises and
  what it signs cannot drift apart.

### Fixed

- *(client)* The signed `PATCH`-with-query builder resolved its URL from the
  gateway base directly instead of going through the shared per-path base
  resolution. Not a live misroute — its only caller (`amend_order`) uses a
  gateway path — but it silently opted out of the centralized `/api/v1` routing
  rule, and would have opted out of the real-funds gate too. Had amend ever
  moved to the v1 surface it would have misrouted, and signed, silently. One
  rule, one place: no builder gets its own base.

- *(client)* Signed requests resolved their base **after** signing, so a request
  refused for its target still drew a nonce. Harmless for the stateless default
  `SystemTimeNonce`, but a caller-supplied monotonic counter would desync against
  the server over a request that was never sent. Bases are now resolved first,
  pinned by a test with a counting nonce across all four signed builder shapes.

- *(docs)* Retired the "production" vocabulary the rename exists to correct: the
  `fund()` refusal message, the `deposit`/`claim_credit` docs, and the
  no-WebSocket-endpoint errors said "production" where they meant *real funds*,
  or "production WS host not yet confirmed" where the host is in fact published
  but not yet usable (it is a different origin from the REST base this network
  still targets, and the origin-scoped upgrade token cannot cross the two —
  ENG-3398).

## [0.7.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.6.1...v0.7.0) - 2026-07-30

### Added

- *(rest)* [**breaking**] send `limit` on GET /fills from fetch_my_trades (ENG-8167) ([#115](https://github.com/nexus-xyz/nexus-exchange-rs/pull/115))
- *(rest)* implement /orders/history, /positions/closed and /account/equity-history (ENG-8148) ([#113](https://github.com/nexus-xyz/nexus-exchange-rs/pull/113))
- *(rest)* return a Paginator from the cursor-paginated endpoints (ENG-8084) ([#112](https://github.com/nexus-xyz/nexus-exchange-rs/pull/112))
- *(orders)* add preview_order for POST /api/v1/orders/preview (ENG-7928) ([#111](https://github.com/nexus-xyz/nexus-exchange-rs/pull/111))
- *(account)* [**breaking**] expose portfolio-parity endpoints/fields in the Rust SDK (ENG-6457) ([#109](https://github.com/nexus-xyz/nexus-exchange-rs/pull/109))
- *(orders)* [**breaking**] surface Order.limit_offset_bps on read-back (ENG-6035) ([#101](https://github.com/nexus-xyz/nexus-exchange-rs/pull/101))

### Fixed

- *(spec-drift)* count endpoints reached only through a paginator (ENG-8166) ([#114](https://github.com/nexus-xyz/nexus-exchange-rs/pull/114))
- *(docs)* correct the SDK<->spec compat table and stop the bot rotting it ([#116](https://github.com/nexus-xyz/nexus-exchange-rs/pull/116))

### Other

- bump dtolnay/rust-toolchain ([#102](https://github.com/nexus-xyz/nexus-exchange-rs/pull/102))
- bump tokio-tungstenite from 0.29.0 to 0.30.0 ([#104](https://github.com/nexus-xyz/nexus-exchange-rs/pull/104))
- bump the cargo-minor group across 1 directory with 6 updates ([#108](https://github.com/nexus-xyz/nexus-exchange-rs/pull/108))

### Added

- Pinned the API spec to `v0.7.2` and exposed the **portfolio-parity** surface
  (ENG-6457). The bump is purely additive over `v0.7.1` — no operation, schema,
  field, or enum member was removed — so it carries no spec-side breakage.
  - **Portfolio time series.** `fetch_portfolio_history` (`GET
    /api/v1/account/portfolio-history`) returning `PortfolioHistory` /
    `PortfolioPoint` — equity, cumulative PnL (deposit-neutral), and cumulative
    volume, oldest first — selected by the new `PortfolioWindow`
    (`day`/`week`/`month`/`all`), which also fixes the server-side downsample
    cadence and point capacity. Both of the request schema's `limit` bounds
    (`minimum: 1`, `maximum: 366`, exposed as `rest::MAX_PORTFOLIO_HISTORY_LIMIT`)
    are enforced locally, so a value the schema forbids is rejected before the
    request is signed or sent — matching the Python SDK and MCP server. The
    parameter's prose note that an over-capacity value is "clamped, not rejected"
    describes server tolerance of non-conforming input, not licence for a client
    to exceed the schema.

    All three `PortfolioHistory` fields are spec-`required` and decode strictly:
    an absent or `null` `window`, `cadence_ms` or `points` fails the decode rather
    than being defaulted. Defaulting any of them would report a figure the server
    never sent — an empty `points` reads as "no history" and charts a flat line, a
    `0` cadence divides by zero in caller arithmetic, and a substituted window
    misstates the span the points cover. Matches the Python SDK's failure modes.

    `window` is typed as an open `String` rather than the `PortfolioWindow` enum,
    so a window added to a later spec still decodes instead of making the whole
    response unreadable, and the served label stays *reportable* — a caller can
    log or display `"quarter"` instead of losing it. `PortfolioWindow::from_wire`
    (or `PortfolioHistory::window_parsed`) maps it onto the enum, returning `None`
    for a value this SDK version cannot name. The request side stays typed and
    closed, and the `spec-drift` gate fails loudly the moment the spec adds a
    member.
  - **Consolidated account state.** `fetch_account_state` (`GET
    /api/v1/account/state`) returning `AccountState` — the portfolio summary
    **and** all open positions from one coherent server-side read, so the
    aggregates cannot tear against the position list the way separate
    summary/positions calls can.
  - **`withdrawable` + portfolio summary.** `fetch_account_summary` (`GET
    /api/v1/account/summary`) returning the new `AccountPortfolioSummary`, whose
    `withdrawable` is the engine-authoritative free margin floored at zero —
    prefer it over `available_margin` when deciding what may leave the account.
    Every field on it is `Option` and defaulted: the spec gives the schema no
    `required` array, so an absent aggregate nulls that one field instead of
    failing the whole `/account/summary` or `/account/state` decode. `None` means
    "not reported", never zero.
  - **Fail-closed reads are distinguishable.** `/account/state` and
    `/account/summary` answer `502 authoritative_margin_unavailable` when the
    engine-authoritative margin view is down, rather than serving a
    locally-estimated balance. That code now survives into the error (see
    *Changed*), and both methods' docs say plainly that such an `Err` means
    "retry the read", **not** "the account is flat".
  - **Account fees.** `fetch_account_fees` (`GET /api/v1/account/fees`)
    returning `AccountFees` — effective maker/taker rate in bps (**signed**: a
    negative maker fee is a rebate), fee tier, rate `schedule` scope, rolling
    30-day volume with a `volume_30d_estimated` undercount flag, and
    `discounts`. `FeeDiscount` keeps the server's object verbatim because the
    spec has not fixed its shape yet. The rates and volume decode strictly: all
    are spec-`required`, and a defaulted fee would read as "trading is free" or a
    defaulted `volume_30d` as "no volume" — figures the server never reported.
    `discounts` is the single documented exception, defaulting to `[]` when absent:
    unlike a time series or a position list, a dropped discount cannot distort a
    figure the caller computes, and matching the Python SDK here means an absent
    `discounts` reports the same way in both. A malformed (non-object) entry still
    fails the decode.
- Extended the `spec-drift` gate's enum invariant to resolve enum-valued
  properties composed by `$ref` / single-branch `allOf`, not just inline `enum`
  arrays. `PortfolioHistory.window` is composed that way, so without this its
  members would have gone unchecked — silently losing the protection the
  invariant exists for. `PortfolioWindow` is now covered.
- *(orders)* `preview_order` (`POST /api/v1/orders/preview`) — pre-trade preview
  projecting an order's margin, equity, liquidation-price, fee and expected-fill
  impact **without submitting it** (ENG-7928). Takes the same `OrderRequest` as
  `create_order`, so preview-then-commit reuses one value; nothing is placed and
  no margin is reserved. Returns the new `OrderPreview`.

  **A rejected preview is `Ok`, not `Err`.** The endpoint answers "what would
  this order do?", so a projection that the order *would* be rejected is a `200`
  with `accepted: false` and a `reject_reason`. Gate submission on
  `OrderPreview::is_accepted()`, never on `Result::is_ok()`. `is_accepted()`
  fails closed: an unreported `accepted` returns `false` rather than waving an
  order through the server never vouched for.

  Every `OrderPreview` field is `Option` and defaulted, because the spec gives
  `PreviewResponse` **no `required` array** — the server may legitimately omit
  any property, and one absent field must not fail the whole decode. `None`
  means "not reported", never zero. Every monetary field is a decimal *string*
  parsed exactly via the `str` adapter, including
  `projected_post_trade_leverage`, which the spec types as `Decimal` (a string)
  even though the `leverage` *request* parameter elsewhere in the API is a JSON
  number. `reject_reason` stays a free-form `String` rather than an enum so a
  reason added server-side cannot break the response, and `OrderPreview` is
  `#[non_exhaustive]` so a later additive spec field is not a breaking change.
  `OrderPreview` is registered in the `spec-drift` gate's model invariant
  (`OrderPreview` ↔ `PreviewResponse`).

  This unblocks `nexus order preview` in the CLI (ENG-7734), which had no way to
  reach the endpoint: the SDK is the CLI's only transport.

### Changed

- [**breaking**] *(account, positions)* `Position` gains the enriched
  per-position risk fields: `leverage`, `notional_value`, `roe`, `margin_used`,
  `max_leverage` — each with a companion `*_error` — plus `funding_paid`, which
  has **no** `*_error` companion (the spec defines none: the server always sends
  a value, `"0"` when nothing has accrued). Every new field is `Option` and
  defaulted, so positions from a server that omits them still decode. A `None`
  risk field never means zero — the server declines to fabricate a value it
  cannot derive and reports the reason in the matching `*_error`; read the pair
  together.

  `Position` was an externally-constructible struct (all-public fields, no
  `#[non_exhaustive]`), so cargo-semver-checks flags the additions via
  `constructible_struct_adds_field`; downstream struct literals of `Position`
  must be updated. **`AccountSummary` (`GET /api/v1/account`) embeds
  `Vec<Position>` and so inherits this change transitively** — code that
  constructs the positions inside an `AccountSummary` literal needs the same
  update. Read-back only: no request shape changed.
- [**breaking**] *(account, positions)* Added `#[non_exhaustive]` to `Position`
  and to the new `AccountPortfolioSummary`, `AccountState`, `AccountFees`,
  `PortfolioHistory` and `PortfolioPoint`, per the `#[non_exhaustive]` policy in
  CONTRIBUTING. Read these off returned values instead of building them with
  struct literals. Taken **now**, inside a bump that is already breaking, because
  the spec marks almost none of these properties required and is openly planning
  more of them (`tier`/`schedule` are open strings, `FeeDiscount` finalizes with
  the fee model, per-market fee rates are a planned follow-up) — every such
  addition would otherwise be another break, and adding the attribute later is
  itself one. `FeeDiscount` is deliberately excluded: it is a transparent wrapper
  over the raw object, which callers may reasonably construct.
- [**breaking**] *(errors)* `TransientError::Unavailable` gains a `code` field
  carrying the server's machine-readable error code, and `Error::code()` now
  returns it for any failure that came from an API response with one. Previously
  every 5xx dropped the parsed code, and since the API's 5xx bodies carry a `code`
  and no `message`, the whole surface for a fail-closed
  `502 authoritative_margin_unavailable` was `service unavailable [502]: ` — a
  caller could not tell it from a deploy blip, though the correct responses differ
  (retry the read vs. treat balances as unknown). Matches the TypeScript and
  Python SDKs, which both preserve the code. Patterns that destructure
  `Unavailable { status, message }` need a `..`; the `Display` text now includes
  the code.

### Notes

- The `v0.7.2` spec also added `cursor` pagination (and the `X-Next-Cursor`
  response header) to five list endpoints, one of which this SDK already
  implements (`/account/equity-history`); the others are `/fills`,
  `/orders/history`, `/positions/closed` and `/markets/{market_id}/trades`. None
  of it is exposed here — this PR is scoped to the portfolio-parity surface, and
  the drift gate does not check parameters, so nothing failed to flag it. Tracked
  as a fleet-wide follow-up (no SDK exposes it yet).

### Fixed

- *(docs)* Corrected the README SDK↔spec compatibility table, which had been
  misreporting every shipped version since `0.3.x`. It claimed `0.3.x` targeted
  `v0.7.1`; `0.3.x` actually shipped against `v0.5.0`, and the `0.4.x`, `0.5.x`
  and `0.6.x` series were missing entirely. Every row is now derived from the
  release tags (`git show <tag>:.api-version`), and the README documents that
  one-liner so the table can be re-checked rather than trusted.

  Root cause: `spec-autobump` ran a second script that advanced the API-spec cell
  of the table's *top* row on every spec bump, on the documented assumption that
  "the next SDK release (release-plz) appends a new top row". Release-plz only
  touches `CHANGELOG.md` / `Cargo.toml` / `Cargo.lock`, so that counterpart never
  existed: the row's spec cell marched `v0.4.0` → `v0.5.0` → `v0.6.0` → `v0.6.2`
  → `v0.7.1` while its SDK label stayed frozen at `0.3.x`. The script is removed
  and the bot now touches only the marker-delimited "currently targets" line —
  matching what `sync_api_version.py` already documented as the split, which the
  second script had been contradicting. A stale table now reads as *incomplete*
  (a missing row) rather than as a confident false claim.

### Fixed

- send content length for empty signed POSTs (ENG-6344) ([#105](https://github.com/nexus-xyz/nexus-exchange-rs/pull/105))

## [0.6.0](https://github.com/nexus-xyz/nexus-exchange-rs/compare/v0.5.1...v0.6.0) - 2026-07-17

### Added

- [**breaking**] bump API spec to v0.7.1 and model the new surface (ENG-6035) ([#99](https://github.com/nexus-xyz/nexus-exchange-rs/pull/99))
- *(client)* send X-Nexus-Api-Version header + confirm normalized User-Agent (ENG-5954) ([#98](https://github.com/nexus-xyz/nexus-exchange-rs/pull/98))
- *(spec-drift)* validate enum members, not just names (ENG-5474) ([#97](https://github.com/nexus-xyz/nexus-exchange-rs/pull/97))

### Fixed

- *(sdk)* decode amend response as bare order (ENG-5947) ([#96](https://github.com/nexus-xyz/nexus-exchange-rs/pull/96))

### Other

- harden spec-drift check: enforce inline-literal paths + guard LOGIN_MESSAGE ([#91](https://github.com/nexus-xyz/nexus-exchange-rs/pull/91))
- bump sha3 from 0.10.9 to 0.12.0 ([#92](https://github.com/nexus-xyz/nexus-exchange-rs/pull/92))
- add AGENTS.md with merge-safety guardrails (ENG-5319) ([#89](https://github.com/nexus-xyz/nexus-exchange-rs/pull/89))

### Added

- Pinned the API spec to `v0.7.1` (ENG-6035) and modeled the new surface so the
  `spec-drift` gate stays green:
  - **Triggerable & trailing order types.** `OrderType` gains `StopLimit`,
    `StopMarket`, `TakeProfitLimit`, `TakeProfitMarket`, `TrailingStop`, and
    `TrailingLimit`. `OrderRequest` gains `trigger_price` (stop / take-profit),
    `trailing_offset_bps` (trailing) and `limit_offset_bps` (`TrailingLimit`),
    with the `OrderRequest::trailing_limit` constructor and `with_trigger_price`
    / `with_trailing_offset_bps` / `with_limit_offset_bps` builders.
  - **Cancel-on-disconnect.** `fetch_cancel_on_disconnect` /
    `set_cancel_on_disconnect` (`GET`/`PUT /api/v1/account/cancel-on-disconnect`)
    returning `CancelOnDisconnectStatus`.
  - **Bridge Phase A (deposits).** `fetch_bridge_assets` (public),
    `create_bridge_deposit_address`, `fetch_bridge_deposit_addresses`,
    `fetch_bridge_deposits`, and `fetch_bridge_deposit` over the host-root
    `/api/v1/bridge/*` surface, with `BridgeAssetsResponse`, `BridgeChainAssets`,
    `BridgeAsset`, `BridgeDepositAddress`, and `BridgeDeposit`.
- *(client)* send an `X-Nexus-Api-Version` header on every request, sourced from
  the pinned `.api-version` spec tag (currently `v0.7.1`) so it never drifts,
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

### Changed

- [**breaking**] `GET /health` and `GET /ready` were removed upstream in
  `v0.7.1`; `Client::health_check` now reads the public `GET /status` aggregate
  and `HealthStatus` is remodeled to match its `ServiceHealth` shape — `status`
  (`ok`/`degraded`/`down`/`starting`), `timestamp_ms`, and an opaque `services`
  — replacing the previous indexer-snapshot fields (`events_received`,
  `fills_total`, `uptime_seconds`, `connected`, `health`).
- [**breaking**] New `OrderType` variants and new public `OrderRequest` fields
  (above) widen those types; downstream exhaustive `match`es on `OrderType` and
  struct literals of `OrderRequest` must be updated.
- [**breaking**] *(orders)* surface `Order.limit_offset_bps` on the order-read
  path (ENG-6035 follow-up), mirroring `OrderRequest::limit_offset_bps`, so a
  read-back `TrailingLimit` order no longer drops its fired-limit-price offset.
  The spec's `Order` response schema already carried the field; it is now modeled
  (the `spec-drift` informational note for `Order` clears). `Order` is an
  externally-constructible struct, so adding a public field is a breaking change
  (struct literals must now set it) — matching how the sibling `OrderRequest`
  field additions were classified in #99.

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
