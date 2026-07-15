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
   self.get/signed_get/signed_post/... helper calls) and assert it equals the
   endpoints.txt set, modulo two explicit, documented allowlists:

     * CODE_ONLY_OPS    — implemented in the client but intentionally NOT in
                          endpoints.txt (ahead-of-spec; they would break the
                          endpoints.txt<->spec check above until the spec ships).
     * NON_REST_TARGETS — listed in endpoints.txt but reached without a REST
                          helper call (e.g. the WebSocket upgrade).

   The check fails if (a) the code implements an op that is neither in
   endpoints.txt nor in CODE_ONLY_OPS, or (b) endpoints.txt lists an op that has
   no implementing method and is not in NON_REST_TARGETS.

   The code parser reads the path *literal* passed inline to each helper call, so
   it relies on an inline-literal convention (every helper call passes its path
   as `"..."` / `&format!("...")` directly, never a path built into a local var
   first). That convention is now ENFORCED with a loud failure — a call site
   whose first argument is not an inline literal aborts the check — so a wrapper
   can no longer silently undercount the implemented set (the #49 review nit).

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
         corresponding spec schema property.
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
HELPER_METHOD = {
    "get": "GET",
    "signed_get": "GET",
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
MODEL_SCHEMA = {
    "Market": "Market",
    "MarketSummary": "MarketSummary",
    "MarketStatus": "MarketStatus",
    "Ticker": "Ticker",
    "OrderBook": "OrderBook",
    "Trade": "Trade",
    "FundingSample": "FundingSample",
    "RateLimitStatus": "RateLimitStatus",
    "AccountSummary": "AccountSummary",
    "Position": "Position",
    "Fill": "Fill",
    "Order": "Order",
    "OrderRequest": "OrderRequest",
    "OrderResponse": "OrderResponse",
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
ENUM_SCHEMA = {
    "Side": ("OrderRequest", "side"),
    "OrderType": ("OrderRequest", "order_type"),
    "TimeInForce": ("OrderRequest", "time_in_force"),
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

# Matches a call site up to (but not into) the first argument: `self.<helper>(`
# plus optional whitespace and an optional `&format!(` wrapper. Whatever follows
# must be a `"..."` literal for the convention to hold.
_HELPER_ALT = "|".join(sorted(HELPER_METHOD, key=len, reverse=True))
_CALL_SITE_RE = re.compile(
    r"self\.(" + _HELPER_ALT + r")"
    r"\s*\(\s*"                   # open paren + optional whitespace
    r"(?:&\s*format!\s*\(\s*)?"   # optional `&format!(` wrapper
)

# Each call is `self.<helper>(` followed (allowing whitespace/newlines, since
# multi-line calls wrap the path onto the next line) by the path argument: either
# a `"..."` literal or `&format!("...")`. We capture the helper name and the
# first string literal that opens the argument list.
_CALL_RE = re.compile(
    r"self\.(" + _HELPER_ALT + r")"
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
            snippet = src[m.start(): m.start() + 60].splitlines()[0]
            non_inline.append((lineno, m.group(1), snippet))
    if non_inline:
        print(
            f"\nERROR: {len(non_inline)} helper call(s) in {path} do not pass "
            f"their path as an inline string literal. The drift parser only sees "
            f"inline `\"...\"` / `&format!(\"...\")` paths; a path built into a "
            f"local variable first would be silently uncounted, undercounting "
            f"implemented ops. Inline the path literal at the call site:"
        )
        for lineno, helper, snippet in non_inline:
            print(f"  - {path}:{lineno}: self.{helper}(...  ->  {snippet.strip()}")
        sys.exit(1)

    ops = set()
    for m in _CALL_RE.finditer(src):
        helper, p = m.group(1), m.group(2)
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


def check_code_vs_targets(targeted):
    """Invariant 2: implemented REST ops == endpoints.txt, modulo the two
    documented allowlists. Returns the number of errors printed."""
    impl = implemented_ops()
    targeted_norm = {(m, normalize_path(p)) for m, p in targeted}

    # (a) implemented but not listed (and not an intentional code-only op).
    impl_missing_from_targets = sorted(impl - targeted_norm - CODE_ONLY_OPS)
    # (b) listed but not implemented (and not an intentional non-REST target).
    targets_without_impl = sorted(targeted_norm - impl - NON_REST_TARGETS)
    # Bonus integrity check: a CODE_ONLY_OPS entry that is no longer implemented
    # is stale and should be removed — catch it so the allowlist can't rot.
    stale_code_only = sorted(CODE_ONLY_OPS - impl)

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


def _apply_rename_all(name, rule):
    """Map a snake_case Rust field identifier to its serde wire name under a
    container `rename_all` rule. Fail closed on an unknown rule rather than
    silently mis-deriving a name (which would manufacture phantom drift)."""
    if rule in (None, "snake_case"):
        return name
    parts = name.split("_")
    if rule == "camelCase":
        return parts[0] + "".join(p[:1].upper() + p[1:] for p in parts[1:])
    if rule == "PascalCase":
        return "".join(p[:1].upper() + p[1:] for p in parts)
    if rule == "SCREAMING_SNAKE_CASE":
        return name.upper()
    if rule == "kebab-case":
        return name.replace("_", "-")
    if rule == "SCREAMING-KEBAB-CASE":
        return name.upper().replace("_", "-")
    if rule == "lowercase":
        return name.replace("_", "").lower()
    if rule == "UPPERCASE":
        return name.replace("_", "").upper()
    sys.exit(
        f"ERROR: unsupported serde rename_all rule {rule!r}; extend "
        f"_apply_rename_all() in {os.path.basename(__file__)}."
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
        fields.add(rn.group(1) if rn else _apply_rename_all(ident, rename_all))
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
        members.add(rn.group(1) if rn else _apply_rename_all(ident, rename_all))
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
        spec_members = prop_schema.get("enum")
        if not spec_members:
            errors += 1
            print(
                f"\nERROR: spec {schema_name!r}.{prop} is no longer an `enum` "
                f"(the member set modeled by SDK `{rust_name}` can't be compared) "
                f"— update ENUM_SCHEMA / the check."
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
    failures += check_code_vs_targets(targeted)

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
