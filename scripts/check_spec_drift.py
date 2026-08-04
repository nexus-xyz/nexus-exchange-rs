#!/usr/bin/env python3
"""Check the SDK's targeted endpoints against the pinned OpenAPI spec AND the
Rust client code.

Five independent invariants are enforced:

1. endpoints.txt <-> spec
   Every endpoint the SDK targets (endpoints.txt) must exist in the pinned
   OpenAPI spec (.api-version). A miss means a breaking change, rename, or typo
   in the spec. Spec operations the SDK does not yet cover are reported as an
   informational coverage gap.

2. client code <-> endpoints.txt   (added by ENG-3868)
   endpoints.txt used to be hand-maintained with no link to the code, so a
   wrapper could be added (or removed) without updating the checklist and this
   check would still pass. We now derive the set of REST operations the client
   actually implements from src/rest.rs (the path-literal arguments to the
   get/signed_get/signed_post/get_page/... helper calls, on either a `self` or a
   `client` receiver — see _RECEIVER_ALT) and assert it equals the endpoints.txt
   set, modulo two explicit, documented allowlists:

     * CODE_ONLY_OPS    — implemented in the client but intentionally NOT in
                          endpoints.txt (ahead-of-spec; they would break the
                          endpoints.txt<->spec check above until the spec ships).
     * NON_REST_TARGETS — listed in endpoints.txt but reached without a REST
                          helper call (e.g. the WebSocket upgrade).

   The check fails if (a) the code implements an op that is neither in
   endpoints.txt nor in CODE_ONLY_OPS, or (b) endpoints.txt lists an op that has
   no implementing method and is not in NON_REST_TARGETS.

   Both allowlists also carry the stale-entry checks the model/enum allowlists
   have, so an exemption cannot quietly outlive its reason (ENG-7961):

     * a CODE_ONLY_OPS entry no longer implemented in the client;
     * a CODE_ONLY_OPS entry the pinned spec now DEFINES — the allowlist means
       "implemented but ahead of the pinned spec", so once the spec catches up
       the op belongs in endpoints.txt. Leaving it parked is the damaging case:
       the op is deliberately kept OUT of endpoints.txt, so invariant 1 stops
       checking that its path still exists, and the SDK's coverage number
       understates reality;
     * a NON_REST_TARGETS entry that is not in endpoints.txt, which therefore
       suppresses nothing and is a stale exemption waiting to hide a regression.

   The code parser reads the path *literal* passed inline to each helper call, so
   it relies on an inline-literal convention (every helper call passes its path
   as `"..."` / `&format!("...")` directly, never a path built into a local var
   first). That convention is now ENFORCED with a loud failure — a call site
   whose first argument is not an inline literal aborts the check — so a wrapper
   can no longer silently undercount the implemented set (the #49 review nit).

   Undercounting is the failure mode this invariant has to be paranoid about, and
   it has bitten once already: the parser was anchored on a `self.` receiver, so
   the cursor-paginated readers — called on an owned `Client` clone inside a
   paginator closure — were invisible, and an endpoint reachable ONLY through a
   paginator would have passed silently unimplemented (ENG-8166). Both the
   receiver set (_RECEIVER_ALT) and the helper set (HELPER_METHOD) are therefore
   explicit and asserted, never inferred: this check exists to catch a gap, so it
   fails loudly rather than reporting green over one.

3. SDK models <-> spec schemas   (added by ENG-3377)
   Operations existing is necessary but not sufficient: the SDK can still drift
   on the *shape* of a payload. A representative set of serde models in
   src/types.rs (MODEL_SCHEMA) is matched field-by-field against the pinned
   spec's component schemas. The check fails when a model reads (or writes) a
   wire field the pinned spec no longer defines — the silent-breakage class the
   `mark_price` -> `last_trade_price` rename (PR #48) was: the field vanishes
   from the spec but the struct keeps deserializing it, so the value just goes
   quietly absent/`None` at runtime. Field names are compared after applying the
   struct's serde renames (`rename_all` + per-field `rename`), so the comparison
   is against the actual wire names, not the Rust identifiers.

   Modulo one documented allowlist, mirroring CODE_ONLY_OPS:

     * MODEL_FIELDS_AHEAD_OF_SPEC — (struct, wire_field) pairs the SDK
                          intentionally carries ahead of the pinned spec.

   Spec fields a model does not surface are reported as an informational gap,
   not a failure: serde ignores unknown fields, so omitting one is
   forward-compatible (the SDK just doesn't expose it yet). Only fields the SDK
   depends on that the spec dropped are breakage. The check is deliberately
   name-existence only (not types / required-ness): the SDK intentionally widens
   spec-required fields to `Option` for forward-compat (see CONTRIBUTING), so a
   stricter comparison would be all false positives.

4. SDK LOGIN_MESSAGE constant <-> spec canonical value   (added by ENG-3918)
   The exact bytes the SDK signs (EIP-191) at login must equal the spec's
   canonical `/auth/login` message; a mismatch silently rejects every login.
   Documented at check_login_message() below.

5. SDK enums <-> spec enums   (added by ENG-5474)
   Invariant 3 compares which *fields* a payload has, but not the *values* an
   enum field may take. An upstream enum can gain a member (PostOnly time-in-
   force, ENG-5058) or the WS protocol a channel (Liquidations, ENG-4646) while
   the name-level checks above stay green — leaving a typed client silently
   unable to express or receive it. Two enum sources are diffed against the
   released spec:

     5a. A representative set of hand-written serde enums in src/types.rs
         (ENUM_SCHEMA) whose *wire* member names (after applying `rename_all` +
         per-variant `rename`; deserialize-only `alias`es are not canonical wire
         values and are excluded) are diffed against the `enum` array of the
         corresponding spec schema property. That property may hold its `enum`
         inline or reach it through a `$ref` / single-branch `allOf` wrapper (the
         idiom for attaching a `default` to a `$ref`); resolve_enum() follows both
         so a factored-out enum is checked, not silently skipped.
     5b. The WebSocket channel set: the wire names the `Channel` enum emits
         (src/ws/protocol.rs `Channel::name`) diffed against the channels the
         spec documents in the `GET /ws` description. WS channels are the one
         enum the spec carries as prose, not a machine-readable `enum` array, so
         5b extracts them from two fixed marker lines and fails LOUDLY (never
         silently skips) if those markers move — see spec_ws_channels().

   Unlike Invariant 3's field check, the enum comparison is BIDIRECTIONAL: BOTH
   a spec member the SDK omits AND an SDK member the spec lacks are failures. A
   spec-only member means the client cannot express/receive a value the API
   defines (the exact PostOnly/Liquidations class); an SDK-only member means the
   client would emit a value the API rejects. (Contrast Invariant 3, where a
   spec field the SDK omits is merely forward-compatible: serde drops unknown
   fields, but it CANNOT invent an enum variant at runtime.)

   Modulo two documented allowlists, mirroring MODEL_FIELDS_AHEAD_OF_SPEC:

     * ENUM_MEMBERS_AHEAD_OF_SPEC   — (enum, wire_member) pairs the SDK models
                            ahead of the pinned spec (5a).
     * WS_CHANNELS_AHEAD_OF_SPEC    — channel names the SDK models ahead of the
                            pinned spec (5b).

   Both allowlists carry the stale-entry check the other allowlists have: an
   entry the spec now defines, or one the SDK no longer models, is flagged so
   the list can't rot.

Usage: check_spec_drift.py <openapi.json>
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
REST_RS = os.path.join(REPO, "src", "rest.rs")
TYPES_RS = os.path.join(REPO, "src", "types.rs")
WS_PROTOCOL_RS = os.path.join(REPO, "src", "ws", "protocol.rs")

# Map each REST helper on `Client` (defined in src/client.rs) to the HTTP method
# it issues. The path is always the first argument: a bare "..." string literal
# or `&format!("...")`. Keep this in sync with the helper set in src/client.rs.
#
# Includes the cursor-paginated readers `get_page` / `signed_get_page` (ENG-8166).
# Those are NOT called on `self`: a `*_paginated` method clones the `Client` and
# the closure calls the helper on the owned clone, so the parser has to match a
# `client.` receiver as well as `self.` (see `_RECEIVER_ALT`) and a turbofish
# (`client.signed_get_page::<Vec<Fill>>(...)`, needed because the closure's return
# type cannot be inferred). Before that, an endpoint reachable ONLY through a
# paginator was invisible here and silently UNDERCOUNTED by invariant 2 — a
# verification tool reporting green over a real gap, which is worse than no tool.
HELPER_METHOD = {
    "get": "GET",
    "get_page": "GET",
    "signed_get": "GET",
    "signed_get_page": "GET",
    "post_unsigned": "POST",
    "signed_post": "POST",
    "signed_post_empty": "POST",
    "signed_put": "PUT",
    "signed_delete": "DELETE",
    "signed_delete_with_query": "DELETE",
    "signed_patch_with_query": "PATCH",
}

# Implemented in src/rest.rs but intentionally absent from endpoints.txt: these
# Tier 3 operations are AHEAD OF the pinned spec, so adding them to endpoints.txt
# would (correctly) fail the endpoints.txt<->spec invariant above until the spec
# ships them. Move a line out of here and into endpoints.txt once the pinned
# spec gains the operation. Paths use the normalized `{}` placeholder form.
CODE_ONLY_OPS = {
    ("POST", "/account/leverage"),       # set_leverage
    ("POST", "/account/margin-mode"),    # set_margin_mode
    ("POST", "/orders/batch-cancel"),    # cancel_orders
    ("GET", "/orders/by-client-id/{}"),  # fetch_order_by_client_id
    ("DELETE", "/orders/by-client-id/{}"),  # cancel_order_by_client_id
    ("GET", "/funding-payments"),        # fetch_funding_payments
    ("POST", "/transfers"),              # create_transfer
    ("GET", "/transfers"),               # fetch_transfers
    ("GET", "/sub-accounts"),            # fetch_sub_accounts
    ("POST", "/sub-accounts"),           # create_sub_account
}

# Listed in endpoints.txt but reached WITHOUT a REST helper call, so the code
# parser cannot (and should not) see it. The WebSocket upgrade is opened by the
# ws client via tokio_tungstenite against the configured ws_base() origin
# (host-root `/ws`, see src/config.rs / src/ws/typed.rs), not a `self.get`. Paths
# use the normalized `{}` placeholder form.
NON_REST_TARGETS = {
    ("GET", "/ws"),
}

# Spec operations that exist but the SDK deliberately does not target. These show
# up in the informational "not yet covered" list and that is fine; documented
# here so the exclusion is intentional, not an oversight:
#   POST /ws-tokens — deprecated; superseded by POST /ws/token.
#   GET  /stream    — deprecated SSE stream; superseded by the /ws upgrade.


# --- Invariant 3: SDK models <-> spec schemas (ENG-3377) ---------------------

# The representative set of SDK serde models checked against the spec, mapping
# each Rust struct in src/types.rs to its OpenAPI component schema name (the two
# names usually match but need not — e.g. AdlEvent <-> AdlEventRecord). Money- and
# auth-critical payloads are prioritized. Add a model here when it gains an
# importance that warrants drift protection; it is intentionally a sample, not an
# exhaustive enumeration of every type.
#
# Intentionally absent: `FeeDiscount`. The spec declares it as a bare
# `additionalProperties: true` object with NO properties (its shape finalizes with
# the fee model), so the field-level comparison has nothing to compare and would
# trip the "no inline properties" guard below. The SDK matches that by keeping the
# payload as a raw map rather than freezing a shape. Add it here once the spec
# gives it real properties.
MODEL_SCHEMA = {
    # Money-critical: carries amount + settlement status for every funds movement.
    "FundsEntry": "FundsEntry",
    "DepositResponse": "DepositResponse",
    # `available_at_ms` is `#[serde(default)]`, so a spec-side rename would not
    # fail the decode — it would silently yield `0`, i.e. the epoch, which reads
    # as "the faucet is claimable now". Registering the model turns that into a
    # CI failure instead of a wrong answer a caller would act on.
    "FaucetResponse": "FaucetResponse",
    "Market": "Market",
    "MarketSummary": "MarketSummary",
    "MarketStatus": "MarketStatus",
    "Ticker": "Ticker",
    "OrderBook": "OrderBook",
    "Trade": "Trade",
    "FundingSample": "FundingSample",
    # Every field on both is `#[serde(default)]` (the venue stats surface reports
    # partially), so a spec-side rename would degrade to a permanent silent
    # `0`/`None`/`""` instead of a decode error. Registering them here is what
    # makes that a CI failure — this invariant's whole purpose.
    "StatsSnapshot": "StatsSnapshot",
    "ThroughputSample": "ThroughputSample",
    "RateLimitStatus": "RateLimitStatus",
    "AccountSummary": "AccountSummary",
    "AccountPortfolioSummary": "AccountPortfolioSummary",
    "AccountState": "AccountState",
    "AccountFees": "AccountFees",
    "PortfolioHistory": "PortfolioHistory",
    "PortfolioPoint": "PortfolioPoint",
    "EquityPoint": "EquityPoint",
    "Position": "Position",
    "ClosedPosition": "ClosedPosition",
    "Fill": "Fill",
    "Order": "Order",
    "OrderHistoryEntry": "OrderHistoryEntry",
    "OrderRequest": "OrderRequest",
    "OrderResponse": "OrderResponse",
    # The Rust name reads better than the spec's schema name; the mapping is why
    # this dict exists (cf. AdlEvent <-> AdlEventRecord).
    "OrderPreview": "PreviewResponse",
    "AgentInfo": "AgentInfo",
    "LoginResponse": "LoginResponse",
    "AdlEvent": "AdlEventRecord",
    "AdlClosure": "AdlClosureRecord",
}

# (Rust struct, wire field) pairs the SDK reads/writes that are intentionally
# AHEAD OF the pinned spec — the model-level analogue of CODE_ONLY_OPS. Without
# this allowlist the field would (correctly) trip the models<->spec invariant
# until the spec ships it. Move an entry out once the pinned spec defines the
# field; a stale entry (field now in the spec) is flagged so the list can't rot.
#   client_order_id — the SDK supports client-assigned order ids (place / look
#     up / cancel by client id) ahead of the pinned spec pinning the field on
#     the Order/OrderRequest schemas.
MODEL_FIELDS_AHEAD_OF_SPEC = {
    ("Order", "client_order_id"),
    ("OrderRequest", "client_order_id"),
}


# --- Invariant 5: SDK enums <-> spec enums (ENG-5474) ------------------------

# 5a. Representative hand-written serde enums in src/types.rs, mapped to the spec
# schema PROPERTY whose `enum` array they mirror: rust_enum -> (schema, property).
# The property is chosen so its casing matches the enum's *canonical* serialized
# form — e.g. `Side` is mapped to OrderRequest.side (`Buy`/`Sell`), the form it
# serializes, not Trade.side (`buy`/`sell`), which it only accepts via `alias` on
# deserialize. Like MODEL_SCHEMA this is a curated sample, not every enum: enums
# with no spec counterpart (e.g. `MarginMode`, whose margin-mode endpoint is
# still a CODE_ONLY_OP ahead of spec) are intentionally omitted.
# The property may carry its `enum` inline OR reach it through a `$ref` / a
# single-branch `allOf` wrapper (see resolve_enum below) — the spec uses the
# latter for `PortfolioHistory.window`, so a `default` can sit alongside the ref.
ENUM_SCHEMA = {
    "FundsKind": ("FundsEntry", "kind"),
    "FundsStatus": ("FundsEntry", "status"),
    "Side": ("OrderRequest", "side"),
    "OrderType": ("OrderRequest", "order_type"),
    "TimeInForce": ("OrderRequest", "time_in_force"),
    "PortfolioWindow": ("PortfolioHistory", "window"),
}

# (rust_enum, wire_member) pairs the SDK models AHEAD OF the pinned spec — the
# enum-level analogue of MODEL_FIELDS_AHEAD_OF_SPEC. Without this an SDK-only
# member would (correctly) trip the bidirectional check until the spec ships it.
# Move an entry out once the pinned spec's enum defines the member; a stale entry
# (member now in the spec, or no longer modeled by the SDK) is flagged so the
# list can't rot. Empty today: the SDK's enum members all match the pinned spec.
ENUM_MEMBERS_AHEAD_OF_SPEC = set()

# 5b. WS channel names the SDK's `Channel` enum emits but the pinned spec's
# `GET /ws` description does not yet list — the WS analogue of the allowlist
# above. Same stale-entry check applies.
WS_CHANNELS_AHEAD_OF_SPEC = set()


def normalize_path(p):
    """Collapse any `{placeholder}` segment to a bare `{}` so a code path like
    `/keys/{id}` (local variable name) matches a spec/endpoints path like
    `/keys/{key_id}`. Path matching is by position, not placeholder name."""
    return re.sub(r"\{[^}]*\}", "{}", p)


def load_targeted(path="endpoints.txt"):
    out = []
    seen = {}
    with open(path) as f:
        for lineno, raw in enumerate(f, 1):
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(None, 1)
            if len(parts) != 2:
                sys.exit(
                    f"ERROR: {path}:{lineno}: expected 'METHOD /path', got {line!r}"
                )
            method, p = parts
            op = (method.upper(), p)
            if op in seen:
                sys.exit(
                    f"ERROR: {path}:{lineno}: duplicate endpoint "
                    f"{op[0]} {op[1]!r} (first seen on line {seen[op]})"
                )
            seen[op] = lineno
            out.append(op)
    return out


def spec_ops(spec):
    ops = set()
    for p, methods in spec.get("paths", {}).items():
        for m in methods:
            if m.lower() in ("get", "post", "put", "delete", "patch"):
                ops.add((m.upper(), p))
    return ops


# The parser derives implemented ops by reading the path *literal* passed inline
# to each helper call, so it depends on an INLINE-LITERAL CONVENTION: every helper
# call in src/rest.rs must pass its path as a bare `"..."` string literal or a
# `&format!("...")` directly in the call — never a path built into a local first
# (`let p = format!(...); self.get(&p, …)`). A non-inline path would be invisible
# to `_CALL_RE` and silently *undercount* implemented ops (and could mis-flag an
# endpoints.txt line as unimplemented). Rather than best-effort guessing at local
# variables, we enforce the convention with a loud failure: `_CALL_SITE_RE` finds
# every call site and `implemented_ops` asserts each one is followed by an inline
# literal, exiting non-zero otherwise. See enforce below.

# Receivers a REST helper is called on in src/rest.rs. `self` covers the ordinary
# `async fn` endpoint methods; `client` covers the cursor-paginated ones, where the
# `*_paginated` method clones the `Client` (`let client = self.clone()`) and the
# page-fetching closure calls the helper on that owned clone — `self` is not
# available inside a `'static` closure. Anchoring on `self` alone made every
# paginator call site invisible (ENG-8166).
#
# It is an explicit alternation, not a wildcard `\w+\.`: a wildcard would also
# match unrelated receivers (and doc-comment prose), and the failure mode we care
# about is *undercounting*, so a new receiver name must be added here deliberately
# — the count-agreement assert at the end of `implemented_ops` and the
# inline-literal enforcement both key off these same regexes, so they cannot
# silently disagree.
#
# The leading `\b` is what makes that alternation real rather than a suffix match.
# Without it the pattern matches *any* receiver ENDING in `self`/`client`, so
# `some_client.get(...)`, `http_client.signed_get(...)` and `myself.get(...)` were
# all counted — a partial wildcard wearing an allowlist's comment (the #114 review
# nit). `\b` holds at a token start but not inside `some_client` (`_` -> `c` is
# word-to-word, so there is no boundary there), which is exactly the distinction
# this wants. The failure direction was *over*counting, so the undercount
# guarantee invariant 2 exists for was never at risk — but the allowlist has to
# mean what its comment says, and `test_unknown_receiver_is_not_counted` pins it.
_RECEIVER_ALT = r"\b(self|client)"

# The method-access dot, allowing the line break rustfmt inserts when a call is
# too long to fit — the paginated calls wrap as
#     let (items, next) = client
#         .signed_get_page::<Vec<Fill>>("/api/v1/fills", &page_query(&req))
# so a regex anchored on a literal `client.` (no whitespace) matches none of them.
# That is not a hypothetical: it is why the first cut of this fix still found zero
# paginator call sites.
_DOT = r"\s*\.\s*"

# Matches a call site up to (but not into) the first argument: `<recv>.<helper>(`
# plus an optional turbofish, optional whitespace, and an optional `&format!(`
# wrapper. Whatever follows must be a `"..."` literal for the convention to hold.
#
# The turbofish is not decoration: a paginator closure returns `Page<T>`, so the
# helper's response type has to be named at the call site
# (`client.signed_get_page::<Vec<Fill>>(...)`). `[^()]*` stops at the paren that
# opens the argument list, so it spans nested generics (`Vec<Fill>`) without
# running past the call.
_HELPER_ALT = "|".join(sorted(HELPER_METHOD, key=len, reverse=True))
_TURBOFISH = r"(?:\s*::\s*<[^()]*>)?"
_CALL_SITE_RE = re.compile(
    _RECEIVER_ALT + _DOT + r"(" + _HELPER_ALT + r")"
    + _TURBOFISH +                # optional `::<T>` on the helper
    r"\s*\(\s*"                   # open paren + optional whitespace
    r"(?:&\s*format!\s*\(\s*)?"   # optional `&format!(` wrapper
)

# Each call is `<recv>.<helper>(` followed (allowing whitespace/newlines, since
# multi-line calls wrap the path onto the next line) by the path argument: either
# a `"..."` literal or `&format!("...")`. Both regexes capture the receiver as
# group 1 and the helper as group 2 (so a diagnostic can name the real call site
# rather than assuming `self.`); `_CALL_RE` captures the path literal as group 3.
_CALL_RE = re.compile(
    _RECEIVER_ALT + _DOT + r"(" + _HELPER_ALT + r")"
    + _TURBOFISH +               # optional `::<T>` on the helper
    r"\s*\(\s*"            # open paren + optional whitespace
    r"(?:&\s*format!\s*\(\s*)?"  # optional `&format!(` wrapper
    r'"([^"]+)"'           # the path string literal
)


def implemented_ops(path=REST_RS):
    """Derive the set of (METHOD, normalized_path) the client implements from the
    path-literal arguments to the REST helper calls in src/rest.rs.

    Enforces the inline-literal convention (see `_CALL_SITE_RE` note): every
    helper call must pass its path inline as `"..."` or `&format!("...")`. A call
    whose first argument is not an inline literal (e.g. a path built into a local
    variable first) would be silently missed by `_CALL_RE`, undercounting the
    implemented set. We fail loudly on any such call so drift can never be
    silently undercounted."""
    try:
        src = open(path).read()
    except OSError as e:
        sys.exit(f"ERROR: cannot read client source {path!r}: {e}")

    # Every call site must be immediately followed by an inline path literal.
    # `_CALL_SITE_RE` matches through the (optional) `&format!(` wrapper; the very
    # next non-space character must open a string literal. If it does not, the
    # path is not inline — reject it rather than silently dropping the op.
    non_inline = []
    for m in _CALL_SITE_RE.finditer(src):
        rest_after = src[m.end():]
        if not rest_after.lstrip().startswith('"'):
            lineno = src.count("\n", 0, m.start()) + 1
            snippet = " ".join(src[m.start(): m.start() + 90].split())[:70]
            non_inline.append((lineno, f"{m.group(1)}.{m.group(2)}", snippet))
    if non_inline:
        print(
            f"\nERROR: {len(non_inline)} helper call(s) in {path} do not pass "
            f"their path as an inline string literal. The drift parser only sees "
            f"inline `\"...\"` / `&format!(\"...\")` paths; a path built into a "
            f"local variable first would be silently uncounted, undercounting "
            f"implemented ops. Inline the path literal at the call site:"
        )
        for lineno, call, snippet in non_inline:
            print(f"  - {path}:{lineno}: {call}(...  ->  {snippet.strip()}")
        sys.exit(1)

    ops = set()
    for m in _CALL_RE.finditer(src):
        helper, p = m.group(2), m.group(3)
        ops.add((HELPER_METHOD[helper], normalize_path(p)))
    if not ops:
        sys.exit(
            f"ERROR: parsed zero REST calls from {path!r}; the helper call "
            f"pattern may have changed — update HELPER_METHOD / the parser."
        )
    # Every call site produced exactly one inline literal (checked above), so the
    # two passes must agree in count. A mismatch means a literal was captured for
    # a site that wasn't matched (or vice versa) — a parser bug; fail loudly.
    n_sites = sum(1 for _ in _CALL_SITE_RE.finditer(src))
    n_literals = sum(1 for _ in _CALL_RE.finditer(src))
    if n_sites != n_literals:
        sys.exit(
            f"ERROR: parser inconsistency in {path}: {n_sites} helper call "
            f"site(s) but {n_literals} inline path literal(s). The call/literal "
            f"regexes have diverged — update the parser."
        )
    return ops


def check_code_vs_targets(targeted, available):
    """Invariant 2: implemented REST ops == endpoints.txt, modulo the two
    documented allowlists. Returns the number of errors printed.

    `available` is the pinned spec's operation set (spec_ops), needed only for
    the CODE_ONLY_OPS staleness check below — an allowlist that means "ahead of
    the pinned spec" cannot be validated without knowing what the spec has."""
    impl = implemented_ops()
    targeted_norm = {(m, normalize_path(p)) for m, p in targeted}
    # spec_ops keeps the spec's own placeholder names; the allowlists are written
    # in normalized `{}` form, so normalize this side before comparing.
    available_norm = {(m, normalize_path(p)) for m, p in available}

    # (a) implemented but not listed (and not an intentional code-only op).
    impl_missing_from_targets = sorted(impl - targeted_norm - CODE_ONLY_OPS)
    # (b) listed but not implemented (and not an intentional non-REST target).
    targets_without_impl = sorted(targeted_norm - impl - NON_REST_TARGETS)
    # Bonus integrity check: a CODE_ONLY_OPS entry that is no longer implemented
    # is stale and should be removed — catch it so the allowlist can't rot.
    stale_code_only = sorted(CODE_ONLY_OPS - impl)
    # The other way CODE_ONLY_OPS rots, and the dangerous one: the pinned spec
    # has CAUGHT UP with an op parked here. The allowlist's whole meaning is
    # "implemented but ahead of the pinned spec"; once the spec declares the op,
    # leaving it parked exempts a real operation from invariant 1 (nothing then
    # checks that its path still exists) and understates the SDK's coverage
    # number, because it is deliberately kept OUT of endpoints.txt.
    landed_code_only = sorted(CODE_ONLY_OPS & available_norm)
    # NON_REST_TARGETS mirrors the same rot risk from the other direction: an
    # entry that is no longer listed in endpoints.txt suppresses nothing and is
    # just a stale exemption waiting to hide a future regression.
    stale_non_rest = sorted(NON_REST_TARGETS - targeted_norm)

    errors = 0
    if impl_missing_from_targets:
        errors += len(impl_missing_from_targets)
        print(
            f"\nERROR: {len(impl_missing_from_targets)} operation(s) implemented "
            f"in src/rest.rs are NOT in endpoints.txt (add them, or add to "
            f"CODE_ONLY_OPS if intentionally ahead of spec):"
        )
        for m, p in impl_missing_from_targets:
            print(f"  - {m} {p}")

    if targets_without_impl:
        errors += len(targets_without_impl)
        print(
            f"\nERROR: {len(targets_without_impl)} endpoints.txt entr(ies) have "
            f"no implementing method in src/rest.rs (remove them, or add to "
            f"NON_REST_TARGETS if reached without a REST helper):"
        )
        for m, p in targets_without_impl:
            print(f"  - {m} {p}")

    if stale_code_only:
        errors += len(stale_code_only)
        print(
            f"\nERROR: {len(stale_code_only)} CODE_ONLY_OPS entr(ies) are no "
            f"longer implemented in src/rest.rs (remove them from the allowlist):"
        )
        for m, p in stale_code_only:
            print(f"  - {m} {p}")

    if landed_code_only:
        errors += len(landed_code_only)
        print(
            f"\nERROR: {len(landed_code_only)} CODE_ONLY_OPS entr(ies) are now "
            f"defined by the pinned spec, so they are no longer 'ahead of spec' "
            f"(move each into endpoints.txt and drop it from the allowlist — "
            f"leaving it parked exempts a real operation and understates coverage):"
        )
        for m, p in landed_code_only:
            print(f"  - {m} {p}")

    if stale_non_rest:
        errors += len(stale_non_rest)
        print(
            f"\nERROR: {len(stale_non_rest)} NON_REST_TARGETS entr(ies) are not "
            f"in endpoints.txt, so they suppress nothing (remove them from the "
            f"allowlist, or re-add the endpoint to endpoints.txt):"
        )
        for m, p in stale_non_rest:
            print(f"  - {m} {p}")

    if not errors:
        print(
            f"\nOK: src/rest.rs implements {len(impl)} REST op(s); all are in "
            f"endpoints.txt or CODE_ONLY_OPS, and every endpoints.txt entry has "
            f"an implementing method or is in NON_REST_TARGETS."
        )
    return errors


# A serde field declaration inside a struct body: any leading attributes (each
# `#[...]` may wrap across lines, but contains no `]`, so `[^\]]*` stays linear —
# no catastrophic backtracking on adversarial input) followed by `pub <name>:`.
_FIELD_RE = re.compile(
    r"((?:#\[[^\]]*\]\s*)*)"            # leading attribute block (possibly empty)
    r"pub\s+([A-Za-z_]\w*)\s*:"         # `pub <field>:`
)
_RENAME_RE = re.compile(r'\brename\s*=\s*"([^"]+)"')
_RENAME_ALL_RE = re.compile(r'\brename_all\s*=\s*"([^"]+)"')
# A field dropped from the wire contract: bare `skip`, `skip_serializing`, or
# `skip_deserializing` — but NOT `skip_serializing_if` (that only omits a `None`,
# the field is still part of the contract). The `\b` after the optional group
# refuses to match the `_if` suffix.
_SKIP_RE = re.compile(r"\bskip(?:_serializing|_deserializing)?\b(?!_if)")


# The `rename_all` rules serde accepts. Shared by both derivations below so an
# unknown rule fails closed in one place rather than per-kind.
_RENAME_ALL_RULES = frozenset(
    {
        "lowercase",
        "UPPERCASE",
        "PascalCase",
        "camelCase",
        "snake_case",
        "SCREAMING_SNAKE_CASE",
        "kebab-case",
        "SCREAMING-KEBAB-CASE",
    }
)


def _pascal_from_snake(field):
    """`mark_price` -> `MarkPrice`. Mirrors serde's field `PascalCase` arm:
    capitalize after every `_` and drop the underscores."""
    pascal = []
    capitalize = True
    for ch in field:
        if ch == "_":
            capitalize = True
        elif capitalize:
            pascal.append(ch.upper())
            capitalize = False
        else:
            pascal.append(ch)
    return "".join(pascal)


def _snake_from_pascal(variant):
    """`PartiallyFilled` -> `partially_filled`. Mirrors serde's variant
    `SnakeCase` arm exactly, including its lack of acronym coalescing: a
    separator goes before *every* interior uppercase char, so `APIKey` becomes
    `a_p_i_key`. That is genuinely what serde emits, so the gate must agree —
    coalescing acronyms here would compute a wire name serde never produces and
    manufacture phantom drift, which is the failure this whole helper exists to
    avoid. A member with an interior acronym run needs an explicit
    `#[serde(rename = "...")]`, which the parser reads in preference to any rule.
    """
    snake = []
    for i, ch in enumerate(variant):
        if i > 0 and ch.isupper():
            snake.append("_")
        snake.append(ch.lower())
    return "".join(snake)


def _apply_rename_all(name, rule, kind):
    """Map a Rust identifier to its serde wire name under a container
    `rename_all` rule. Fail closed on an unknown rule rather than silently
    mis-deriving a name (which would manufacture phantom drift).

    `kind` selects the derivation and is **not** a formality: serde applies two
    genuinely different algorithms, `apply_to_field` and `apply_to_variant`
    (`serde_derive/src/internals/case.rs`, pinned at 1.0.229 in `Cargo.lock`).
    They disagree on real inputs, so one shared table cannot serve both:

    - A **field** arrives snake_case and is already a word sequence, so serde
      treats `lowercase` and `snake_case` as identity and only ever rewrites the
      separators. `lowercase` therefore *keeps* the underscores:
      `mark_price` -> `mark_price`, not `markprice`.
    - A **variant** arrives PascalCase with no separators, so serde must split
      it — and `lowercase` there is a plain `to_ascii_lowercase`, which does
      strip the word boundaries: `PartiallyFilled` -> `partiallyfilled`.

    Deriving both from a shared lowercase word list gets the field side of that
    pair wrong (and mis-handles acronyms on the variant side). This function is
    a transcription of serde's two matches rather than a generalization of them,
    so it can be diffed against `case.rs` directly.
    """
    if rule is None:
        return name
    if rule not in _RENAME_ALL_RULES:
        sys.exit(
            f"ERROR: unsupported serde rename_all rule {rule!r}; extend "
            f"_apply_rename_all() in {os.path.basename(__file__)}."
        )
    if kind == "field":
        # serde: `apply_to_field`.
        if rule in ("lowercase", "snake_case"):
            return name
        if rule in ("UPPERCASE", "SCREAMING_SNAKE_CASE"):
            return name.upper()
        if rule == "PascalCase":
            return _pascal_from_snake(name)
        if rule == "camelCase":
            pascal = _pascal_from_snake(name)
            return pascal[:1].lower() + pascal[1:]
        if rule == "kebab-case":
            return name.replace("_", "-")
        if rule == "SCREAMING-KEBAB-CASE":
            return name.upper().replace("_", "-")
    if kind == "variant":
        # serde: `apply_to_variant`.
        if rule == "PascalCase":
            return name
        if rule == "lowercase":
            return name.lower()
        if rule == "UPPERCASE":
            return name.upper()
        if rule == "camelCase":
            # Note: serde lowercases only the first character here; it does
            # *not* re-split the identifier.
            return name[:1].lower() + name[1:]
        if rule == "snake_case":
            return _snake_from_pascal(name)
        if rule == "SCREAMING_SNAKE_CASE":
            return _snake_from_pascal(name).upper()
        if rule == "kebab-case":
            return _snake_from_pascal(name).replace("_", "-")
        if rule == "SCREAMING-KEBAB-CASE":
            return _snake_from_pascal(name).upper().replace("_", "-")
    sys.exit(
        f"ERROR: _apply_rename_all() called with unknown kind {kind!r}; "
        f"expected 'field' or 'variant'."
    )


def parse_model_fields(src, rust_name):
    """Return the set of serde *wire* field names a named-field struct in
    src/types.rs reads/writes. Exits (fail closed) if the struct can't be found
    or parses to zero fields — a renamed/restructured model must surface as a
    loud failure, never as a silently-skipped check."""
    sm = re.search(r"(?m)^(?:pub )?struct " + re.escape(rust_name) + r"\b", src)
    if not sm:
        sys.exit(
            f"ERROR: model {rust_name!r} (in MODEL_SCHEMA) not found as a struct "
            f"in {TYPES_RS}; was it renamed or made a tuple/enum? Update "
            f"MODEL_SCHEMA / the parser."
        )

    # Container `rename_all`: walk upward over this struct's own attribute/doc
    # lines only, stopping at the first line that is neither — so we can't pick
    # up a preceding item's `rename_all`.
    rename_all = None
    for line in reversed(src[: sm.start()].splitlines()):
        s = line.strip()
        if s.startswith(("#[", "///", "//")) or s == "":
            m = _RENAME_ALL_RE.search(s)
            if m:
                rename_all = m.group(1)
        else:
            break

    # Body between the matching braces. Struct bodies hold no nested `{}` (field
    # types use `<>`/`()`), but count depth anyway so this stays correct if that
    # ever changes.
    open_brace = src.index("{", sm.start())
    depth = 0
    body = None
    for j in range(open_brace, len(src)):
        if src[j] == "{":
            depth += 1
        elif src[j] == "}":
            depth -= 1
            if depth == 0:
                body = src[open_brace + 1 : j]
                break
    if body is None:
        sys.exit(f"ERROR: unterminated struct body for {rust_name!r} in {TYPES_RS}.")

    fields = set()
    for attrs, ident in _FIELD_RE.findall(body):
        if _SKIP_RE.search(attrs):
            continue
        rn = _RENAME_RE.search(attrs)
        fields.add(
            rn.group(1) if rn else _apply_rename_all(ident, rename_all, "field")
        )
    if not fields:
        sys.exit(
            f"ERROR: parsed zero fields from struct {rust_name!r} in {TYPES_RS}; "
            f"the field-parsing pattern may have changed — update parse_model_fields()."
        )
    return fields


def check_models_vs_spec(spec):
    """Invariant 3: a representative set of SDK models must not read/write a wire
    field the pinned spec no longer defines. Returns the number of errors printed."""
    schemas = spec.get("components", {}).get("schemas", {})
    try:
        src = open(TYPES_RS).read()
    except OSError as e:
        sys.exit(f"ERROR: cannot read model source {TYPES_RS!r}: {e}")

    errors = 0
    for rust_name, schema_name in sorted(MODEL_SCHEMA.items()):
        schema = schemas.get(schema_name)
        if schema is None:
            errors += 1
            print(
                f"\nERROR: spec schema {schema_name!r} (modeled by SDK "
                f"`{rust_name}`) is absent from the pinned spec (renamed/removed?)."
            )
            continue
        spec_fields = set(schema.get("properties", {}).keys())
        if not spec_fields:
            errors += 1
            print(
                f"\nERROR: spec schema {schema_name!r} has no inline properties "
                f"(composed via $ref/allOf, or shape changed); the field-level "
                f"comparison for `{rust_name}` can't run — update the check."
            )
            continue

        sdk_fields = parse_model_fields(src, rust_name)
        ahead = {f for (r, f) in MODEL_FIELDS_AHEAD_OF_SPEC if r == rust_name}

        # Divergence (failure): the SDK depends on a field the spec dropped and
        # that is not an intentional ahead-of-spec field.
        drifted = sorted(sdk_fields - spec_fields - ahead)
        # Stale allowlist (failure): an ahead-of-spec field the spec now defines.
        landed = sorted(ahead & spec_fields)
        # Coverage gap (informational): spec fields the SDK does not surface.
        uncovered = sorted(spec_fields - sdk_fields)

        if drifted:
            errors += len(drifted)
            print(
                f"\nERROR: SDK model `{rust_name}` reads/writes {len(drifted)} "
                f"field(s) absent from spec schema {schema_name!r} (spec "
                f"renamed/removed them, or add to MODEL_FIELDS_AHEAD_OF_SPEC if "
                f"intentionally ahead of spec):"
            )
            for f in drifted:
                print(f"  - {f}")
        if landed:
            errors += len(landed)
            print(
                f"\nERROR: {len(landed)} MODEL_FIELDS_AHEAD_OF_SPEC entr(ies) for "
                f"`{rust_name}` are now defined by spec schema {schema_name!r}; "
                f"remove them from the allowlist (no longer ahead of spec):"
            )
            for f in landed:
                print(f"  - {f}")
        if uncovered:
            print(
                f"\n`{rust_name}` does not surface {len(uncovered)} field(s) from "
                f"spec schema {schema_name!r} (informational):"
            )
            for f in uncovered:
                print(f"  - {f}")

    if not errors:
        print(
            f"\nOK: all {len(MODEL_SCHEMA)} representative model(s) read/write only "
            f"fields the pinned spec defines (or fields in "
            f"MODEL_FIELDS_AHEAD_OF_SPEC)."
        )
    return errors


# --- Invariant 4: LOGIN_MESSAGE constant <-> spec canonical value (ENG-3918) --

# The SDK constant, e.g. `pub const LOGIN_MESSAGE: &str = "Sign in to Nexus
# Exchange";`. Capture the string literal value. Kept simple: a single, plain
# ASCII literal (no escapes / raw strings) — assert that assumption below.
_LOGIN_MESSAGE_RE = re.compile(
    r'\bconst\s+LOGIN_MESSAGE\s*:\s*&(?:\'\w+\s+)?str\s*=\s*"([^"\\]*)"\s*;'
)


def sdk_login_message(path=REST_RS):
    """Extract the SDK's LOGIN_MESSAGE constant value from src/rest.rs. Fails
    closed if the constant is missing or not a plain string literal."""
    try:
        src = open(path).read()
    except OSError as e:
        sys.exit(f"ERROR: cannot read client source {path!r}: {e}")
    m = _LOGIN_MESSAGE_RE.search(src)
    if not m:
        # Distinguish "gone/renamed" from "shape changed" for a clearer failure.
        if re.search(r"\bLOGIN_MESSAGE\b", src):
            sys.exit(
                f"ERROR: found LOGIN_MESSAGE in {path} but could not parse it as a "
                f"plain `const LOGIN_MESSAGE: &str = \"...\";` (raw string, escape, "
                f"or new shape?) — update _LOGIN_MESSAGE_RE."
            )
        sys.exit(
            f"ERROR: LOGIN_MESSAGE constant not found in {path} (renamed/removed?) "
            f"— it is a cross-repo contract; update the guard if it moved."
        )
    return m.group(1)


def spec_login_message(spec):
    """Extract the canonical login message from the pinned spec. Primary source
    is the `/auth/login` request example's `message` field; falls back to the
    LoginRequest.message description ('Must be exactly: \"...\"'). Fails closed if
    neither is present so the guard can't silently no-op."""
    # Primary: the request-body example on POST /auth/login.
    try:
        example = (
            spec["paths"]["/auth/login"]["post"]["requestBody"]["content"]
            ["application/json"]["example"]
        )
        if isinstance(example, dict) and isinstance(example.get("message"), str):
            return example["message"]
    except (KeyError, TypeError):
        pass

    # Fallback: LoginRequest.message description, e.g. Must be exactly: "...".
    try:
        desc = (
            spec["components"]["schemas"]["LoginRequest"]
            ["properties"]["message"]["description"]
        )
        m = re.search(r'exactly:\s*"([^"]+)"', desc)
        if m:
            return m.group(1)
    except (KeyError, TypeError):
        pass

    sys.exit(
        "ERROR: could not find the canonical login message in the pinned spec "
        "(POST /auth/login request example `message`, nor LoginRequest.message "
        "'Must be exactly: \"...\"' description). The spec shape changed — update "
        "spec_login_message()."
    )


def check_login_message(spec):
    """Invariant 4: the SDK's LOGIN_MESSAGE constant must equal the spec's
    canonical login message. Returns the number of errors printed."""
    sdk = sdk_login_message()
    canonical = spec_login_message(spec)
    if sdk != canonical:
        print(
            f"\nERROR: LOGIN_MESSAGE drift — the SDK constant does not match the "
            f"pinned spec's canonical login message:\n"
            f"  SDK  (src/rest.rs): {sdk!r}\n"
            f"  spec (/auth/login): {canonical!r}\n"
            f"These bytes are EIP-191 signed at login; a mismatch means every SDK "
            f"login is rejected. Update LOGIN_MESSAGE to match the spec (and the "
            f"server), or re-pin .api-version if the spec regressed."
        )
        return 1
    print(
        f"\nOK: SDK LOGIN_MESSAGE matches the pinned spec's canonical login "
        f"message ({canonical!r})."
    )
    return 0


# --- Invariant 5: SDK enums <-> spec enums (ENG-5474) ------------------------

# A single enum variant: an optional leading attribute block (same linear
# `[^\]]*` form as _FIELD_RE, so no catastrophic backtracking) then the variant
# identifier, then its terminator. The body is scanned with a trailing "," (see
# parse_enum_members) so every variant — including the last — ends in one of
# these, letting us tell a plain unit variant (`Gtc,`) from a struct/tuple
# variant (`Placed {` / `Wrapped(`), which this string-enum check does not model.
_ENUM_VARIANT_RE = re.compile(
    r"((?:#\[[^\]]*\]\s*)*)"       # leading attribute block (possibly empty)
    r"([A-Za-z_]\w*)"             # variant identifier
    r"\s*([,{(=])"                # terminator: , (unit) | { ( (data) | = (discriminant)
)
# A `Channel::name()` match arm's wire literal: `... => "trades",`.
_WS_ARM_RE = re.compile(r"=>\s*\"([^\"]+)\"")


def _strip_line_comments(s):
    """Remove `//`-to-end-of-line comments that are NOT inside a string literal.
    Enum variants carry no keyword like a struct field's `pub`, so doc/line
    comment prose (`/// returns a tuple (x, y)`) would otherwise be mis-scanned as
    variants. String contents are preserved so serde `rename = "..."` survives."""
    out = []
    i, n = 0, len(s)
    in_str = False
    while i < n:
        c = s[i]
        if in_str:
            out.append(c)
            if c == "\\" and i + 1 < n:  # keep escaped char (e.g. \") intact
                out.append(s[i + 1])
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
        elif c == '"':
            in_str = True
            out.append(c)
            i += 1
        elif c == "/" and i + 1 < n and s[i + 1] == "/":
            while i < n and s[i] != "\n":  # drop to end of line
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def _enum_body(src, rust_name):
    """Return (body, rename_all) for `enum <rust_name>` in `src`, or exit (fail
    closed) if it is not found. `body` is the text between the enum's braces with
    line comments stripped; `rename_all` is the container serde rule or None."""
    em = re.search(
        r"(?m)^(?:pub(?:\([^)]*\))? )?enum " + re.escape(rust_name) + r"\b", src
    )
    if not em:
        sys.exit(
            f"ERROR: enum {rust_name!r} (in ENUM_SCHEMA) not found in the SDK "
            f"sources; was it renamed or made a struct? Update ENUM_SCHEMA / the parser."
        )

    # Container `rename_all`: walk upward over this enum's own attribute/doc lines
    # only, stopping at the first line that is neither (so a preceding item's
    # rename_all can't leak in) — same approach as parse_model_fields().
    rename_all = None
    for line in reversed(src[: em.start()].splitlines()):
        s = line.strip()
        if s.startswith(("#[", "///", "//")) or s == "":
            m = _RENAME_ALL_RE.search(s)
            if m:
                rename_all = m.group(1)
        else:
            break

    open_brace = src.index("{", em.start())
    depth = 0
    body = None
    for j in range(open_brace, len(src)):
        if src[j] == "{":
            depth += 1
        elif src[j] == "}":
            depth -= 1
            if depth == 0:
                body = src[open_brace + 1 : j]
                break
    if body is None:
        sys.exit(f"ERROR: unterminated enum body for {rust_name!r} in the SDK sources.")
    return _strip_line_comments(body), rename_all


def parse_enum_members(src, rust_name):
    """Return the set of serde *wire* member names a unit-only serde enum in
    src/types.rs serializes to: per-variant `rename` wins, else the container
    `rename_all` maps the identifier; deserialize-only `alias`es are excluded.
    Exits (fail closed) if the enum is missing, has a non-unit (struct/tuple)
    variant this check does not model, or parses to zero members — a
    renamed/restructured enum must be a loud failure, never a silent skip."""
    body, rename_all = _enum_body(src, rust_name)
    members = set()
    # Trailing "," so the final variant is terminated like the rest.
    for attrs, ident, term in _ENUM_VARIANT_RE.findall(body + ","):
        if term in "{(":
            sys.exit(
                f"ERROR: enum {rust_name!r} has a non-unit variant {ident!r} "
                f"(struct/tuple); the enum-member check only models plain unit "
                f"enums. Remove it from ENUM_SCHEMA or extend parse_enum_members()."
            )
        rn = _RENAME_RE.search(attrs)
        members.add(
            rn.group(1) if rn else _apply_rename_all(ident, rename_all, "variant")
        )
    if not members:
        sys.exit(
            f"ERROR: parsed zero members from enum {rust_name!r}; the variant "
            f"pattern may have changed — update parse_enum_members()."
        )
    return members


def parse_ws_channel_names(path=WS_PROTOCOL_RS):
    """Return the set of WS channel wire names the `Channel` enum emits, read from
    its `name()` match arms in src/ws/protocol.rs. Uses name() (the actual wire
    source) rather than the variant identifiers, since Channel's wire names are
    hand-mapped, not serde-derived. Exits (fail closed) if the method or its arms
    can't be found."""
    try:
        src = open(path).read()
    except OSError as e:
        sys.exit(f"ERROR: cannot read WS protocol source {path!r}: {e}")
    fn = re.search(r"fn name\(&self\)\s*->\s*&'static str\s*\{", src)
    if not fn:
        sys.exit(
            f"ERROR: could not find `Channel::name()` in {path!r}; the WS channel "
            f"wire-name source may have changed — update parse_ws_channel_names()."
        )
    open_brace = src.index("{", fn.start())
    depth = 0
    block = None
    for j in range(open_brace, len(src)):
        if src[j] == "{":
            depth += 1
        elif src[j] == "}":
            depth -= 1
            if depth == 0:
                block = src[open_brace + 1 : j]
                break
    if block is None:
        sys.exit(f"ERROR: unterminated `Channel::name()` body in {path!r}.")
    names = set(_WS_ARM_RE.findall(block))
    if not names:
        sys.exit(
            f"ERROR: parsed zero channel names from `Channel::name()` in {path!r}; "
            f"the match-arm pattern may have changed — update parse_ws_channel_names()."
        )
    return names


def spec_ws_channels(spec):
    """Return the set of WS channel names the spec documents in its `GET /ws`
    description. WS channels are the one enum the spec carries as prose rather
    than a machine-readable `enum` array, so we extract them from the two fixed
    marker lines ("**Public channels** ...: `a`, `b`" / "**Per-account channels**
    ...: `c`, `d`"). This couples the check to that phrasing on purpose: it fails
    LOUDLY if a marker moves (so a maintainer re-derives it at spec-pin time)
    rather than silently passing on an empty set. Exits (fail closed) on either."""
    try:
        desc = spec["paths"]["/ws"]["get"]["description"]
    except (KeyError, TypeError):
        sys.exit(
            "ERROR: spec has no `GET /ws` description to read WS channels from; "
            "the WebSocket documentation shape changed — update spec_ws_channels()."
        )
    channels = set()
    for marker in ("Public channels", "Per-account channels"):
        m = re.search(r"\*\*" + marker + r"\*\*[^:]*:(.*)", desc)
        if not m:
            sys.exit(
                f"ERROR: could not find the '{marker}' line in the spec `GET /ws` "
                f"description; the WS channel documentation was reworded — update "
                f"spec_ws_channels() (and re-check the `Channel` enum by hand)."
            )
        # Only the leading list, before any trailing prose ("— each requires a
        # `market` field"), is the channel set; `market` et al. must not leak in.
        # Cut at an em-dash, a spaced hyphen, or a sentence break, whichever the
        # phrasing uses.
        segment = re.split(r"\s—\s|\s-\s|\.\s", m.group(1))[0]
        channels |= set(re.findall(r"`([a-z_]+)`", segment))
    if not channels:
        sys.exit(
            "ERROR: parsed zero WS channels from the spec `GET /ws` description; "
            "its formatting changed — update spec_ws_channels()."
        )
    return channels


def _report_enum_delta(label, sdk_members, spec_members, ahead, ahead_desc):
    """Shared bidirectional enum diff + reporting for 5a/5b. Returns error count.
    `ahead` is the SDK-ahead-of-spec allowlist (members expected in the SDK but
    not the spec); `ahead_desc` names it for the stale-entry messages."""
    # Spec defines a member the SDK does not model -> FAIL. serde cannot invent a
    # variant at runtime, so (unlike a missing struct field) this is real breakage
    # — the client can neither send nor decode the value. This is the PostOnly /
    # Liquidations regression class.
    missing_from_sdk = sorted(spec_members - sdk_members)
    # SDK models a member the spec does not define (and it is not allowlisted) ->
    # FAIL: the client would emit a value the API rejects.
    extra_in_sdk = sorted(sdk_members - spec_members - ahead)
    # Stale allowlist (FAIL): a member the spec now defines...
    landed = sorted(ahead & spec_members)
    # ...or one the SDK no longer models (so the entry protects nothing).
    stale_unmodeled = sorted(ahead - sdk_members)

    errors = 0
    if missing_from_sdk:
        errors += len(missing_from_sdk)
        print(
            f"\nERROR: {label} is missing {len(missing_from_sdk)} member(s) the "
            f"pinned spec defines (add the variant(s) to the SDK enum):"
        )
        for m in missing_from_sdk:
            print(f"  - {m}")
    if extra_in_sdk:
        errors += len(extra_in_sdk)
        print(
            f"\nERROR: {label} models {len(extra_in_sdk)} member(s) absent from "
            f"the pinned spec (spec renamed/removed them, or add to {ahead_desc} "
            f"if intentionally ahead of spec):"
        )
        for m in extra_in_sdk:
            print(f"  - {m}")
    if landed:
        errors += len(landed)
        print(
            f"\nERROR: {len(landed)} {ahead_desc} entr(ies) for {label} are now "
            f"defined by the pinned spec; remove them (no longer ahead of spec):"
        )
        for m in landed:
            print(f"  - {m}")
    if stale_unmodeled:
        errors += len(stale_unmodeled)
        print(
            f"\nERROR: {len(stale_unmodeled)} {ahead_desc} entr(ies) for {label} "
            f"are no longer modeled by the SDK; remove them from the allowlist:"
        )
        for m in stale_unmodeled:
            print(f"  - {m}")
    return errors


# Maximum `$ref` hops followed when resolving a property to its `enum` array. The
# bound makes a malformed spec with a `$ref` cycle fail as "unresolvable" instead
# of spinning forever; real specs need one or two hops.
_MAX_REF_HOPS = 8

# Only component schemas are resolvable targets; a `$ref` pointing anywhere else
# (an external file, a `#/components/parameters/...`) is out of scope here.
_SCHEMA_REF_PREFIX = "#/components/schemas/"


def resolve_enum(schemas, node):
    """Return the `enum` member list a property schema resolves to, or None if it
    carries none.

    Enum-valued properties are not always inline: the spec composes some by
    reference — `PortfolioHistory.window` is `allOf: [{$ref: PortfolioWindow}]`,
    the idiom for attaching a sibling `default`/`description` to a `$ref`. Without
    resolution such a property looks like "not an enum" and its members would go
    unchecked, silently losing the Invariant-5 protection for exactly the enums the
    spec factors out. Follows a direct `$ref` and a single-branch `allOf`, bounded
    by _MAX_REF_HOPS and guarded against a `$ref` cycle."""
    seen = set()
    for _ in range(_MAX_REF_HOPS):
        if not isinstance(node, dict):
            return None
        if node.get("enum"):
            return node["enum"]
        ref = node.get("$ref")
        if ref is None:
            # A one-branch `allOf` is the compose-with-a-`default` idiom. Two or
            # more branches is a real intersection this check has no basis to
            # collapse into one member set, so leave it unresolved and let the
            # caller report it loudly rather than guess.
            branches = node.get("allOf")
            if not isinstance(branches, list) or len(branches) != 1:
                return None
            node = branches[0]
            continue
        if not isinstance(ref, str) or not ref.startswith(_SCHEMA_REF_PREFIX):
            return None
        if ref in seen:  # cycle
            return None
        seen.add(ref)
        node = schemas.get(ref[len(_SCHEMA_REF_PREFIX):])
    return None


def check_enums_vs_spec(spec):
    """Invariant 5a: a representative set of src/types.rs serde enums must model
    exactly the member set of their spec schema property's `enum` array (modulo
    ENUM_MEMBERS_AHEAD_OF_SPEC). Returns the number of errors printed."""
    schemas = spec.get("components", {}).get("schemas", {})
    try:
        src = open(TYPES_RS).read()
    except OSError as e:
        sys.exit(f"ERROR: cannot read model source {TYPES_RS!r}: {e}")

    errors = 0
    for rust_name, (schema_name, prop) in sorted(ENUM_SCHEMA.items()):
        schema = schemas.get(schema_name)
        if schema is None:
            errors += 1
            print(
                f"\nERROR: spec schema {schema_name!r} (carrying the enum modeled "
                f"by SDK `{rust_name}`) is absent from the pinned spec (renamed/removed?)."
            )
            continue
        prop_schema = schema.get("properties", {}).get(prop)
        if prop_schema is None:
            errors += 1
            print(
                f"\nERROR: spec schema {schema_name!r} has no property {prop!r} "
                f"(the enum modeled by SDK `{rust_name}`); it was renamed/removed "
                f"— update ENUM_SCHEMA."
            )
            continue
        spec_members = resolve_enum(schemas, prop_schema)
        if not spec_members:
            errors += 1
            print(
                f"\nERROR: spec {schema_name!r}.{prop} does not resolve to an "
                f"`enum` (the member set modeled by SDK `{rust_name}` can't be "
                f"compared); it is no longer an enum, or is composed in a way "
                f"resolve_enum() does not follow — update ENUM_SCHEMA / the check."
            )
            continue

        sdk_members = parse_enum_members(src, rust_name)
        ahead = {m for (r, m) in ENUM_MEMBERS_AHEAD_OF_SPEC if r == rust_name}
        errors += _report_enum_delta(
            f"SDK enum `{rust_name}` (spec {schema_name!r}.{prop})",
            sdk_members,
            set(spec_members),
            ahead,
            "ENUM_MEMBERS_AHEAD_OF_SPEC",
        )

    if not errors:
        print(
            f"\nOK: all {len(ENUM_SCHEMA)} representative SDK enum(s) model exactly "
            f"the pinned spec's member set (or members in ENUM_MEMBERS_AHEAD_OF_SPEC)."
        )
    return errors


def check_ws_channels_vs_spec(spec):
    """Invariant 5b: the WS `Channel` enum must emit exactly the channels the spec
    documents in `GET /ws` (modulo WS_CHANNELS_AHEAD_OF_SPEC). Returns error count."""
    sdk_channels = parse_ws_channel_names()
    spec_channels = spec_ws_channels(spec)
    errors = _report_enum_delta(
        "WS `Channel` enum (spec `GET /ws`)",
        sdk_channels,
        spec_channels,
        set(WS_CHANNELS_AHEAD_OF_SPEC),
        "WS_CHANNELS_AHEAD_OF_SPEC",
    )
    if not errors:
        print(
            f"\nOK: the WS `Channel` enum emits exactly the {len(spec_channels)} "
            f"channel(s) the pinned spec documents (or channels in "
            f"WS_CHANNELS_AHEAD_OF_SPEC)."
        )
    return errors


def main():
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <openapi.json>")
    with open(sys.argv[1]) as f:
        spec = json.load(f)
    version = spec.get("info", {}).get("version", "?")
    targeted = load_targeted()
    available = spec_ops(spec)

    missing = [op for op in targeted if op not in available]
    uncovered = sorted(available - set(targeted))

    print(f"Spec version: {version}")
    print(f"SDK targets {len(targeted)} endpoints; spec has {len(available)}.")

    if uncovered:
        print(f"\nNot yet covered by the SDK ({len(uncovered)}):")
        for m, p in uncovered:
            print(f"  - {m} {p}")

    failures = 0
    if missing:
        failures += len(missing)
        print(f"\nERROR: {len(missing)} targeted endpoint(s) are NOT in the spec "
              f"(removed/renamed/typo):")
        for m, p in missing:
            print(f"  - {m} {p}")
    else:
        print("\nOK: every targeted endpoint exists in the pinned spec.")

    # Invariant 2: client code <-> endpoints.txt.
    failures += check_code_vs_targets(targeted, available)

    # Invariant 3: SDK models <-> spec schemas.
    failures += check_models_vs_spec(spec)

    # Invariant 4: SDK LOGIN_MESSAGE constant <-> spec canonical value.
    failures += check_login_message(spec)

    # Invariant 5: SDK enums <-> spec enums (5a serde enums, 5b WS channels).
    failures += check_enums_vs_spec(spec)
    failures += check_ws_channels_vs_spec(spec)

    if failures:
        sys.exit(1)


if __name__ == "__main__":
    main()
