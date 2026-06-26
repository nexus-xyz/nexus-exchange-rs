#!/usr/bin/env python3
"""Check the SDK's targeted endpoints against the pinned OpenAPI spec AND the
Rust client code.

Three independent invariants are enforced:

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
}

# Implemented in src/rest.rs but intentionally absent from endpoints.txt: these
# Tier 3 operations are AHEAD OF the pinned spec, so adding them to endpoints.txt
# would (correctly) fail the endpoints.txt<->spec invariant above until the spec
# ships them. Move a line out of here and into endpoints.txt once the pinned
# spec gains the operation. Paths use the normalized `{}` placeholder form.
CODE_ONLY_OPS = {
    ("POST", "/account/leverage"),       # set_leverage
    ("POST", "/account/margin-mode"),    # set_margin_mode
    ("PUT", "/orders/{}"),               # amend_order
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


# Each call is `self.<helper>(` followed (allowing whitespace/newlines, since
# multi-line calls wrap the path onto the next line) by the path argument: either
# a `"..."` literal or `&format!("...")`. We capture the helper name and the
# first string literal that opens the argument list.
_CALL_RE = re.compile(
    r"self\.(" + "|".join(sorted(HELPER_METHOD, key=len, reverse=True)) + r")"
    r"\s*\(\s*"            # open paren + optional whitespace
    r"(?:&\s*format!\s*\(\s*)?"  # optional `&format!(` wrapper
    r'"([^"]+)"'           # the path string literal
)


def implemented_ops(path=REST_RS):
    """Derive the set of (METHOD, normalized_path) the client implements from the
    path-literal arguments to the REST helper calls in src/rest.rs."""
    try:
        src = open(path).read()
    except OSError as e:
        sys.exit(f"ERROR: cannot read client source {path!r}: {e}")
    ops = set()
    for m in _CALL_RE.finditer(src):
        helper, p = m.group(1), m.group(2)
        ops.add((HELPER_METHOD[helper], normalize_path(p)))
    if not ops:
        sys.exit(
            f"ERROR: parsed zero REST calls from {path!r}; the helper call "
            f"pattern may have changed — update HELPER_METHOD / the parser."
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

    if failures:
        sys.exit(1)


if __name__ == "__main__":
    main()
