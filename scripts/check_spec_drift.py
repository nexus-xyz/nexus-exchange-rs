#!/usr/bin/env python3
"""Check the SDK's targeted endpoints against the pinned OpenAPI spec AND the
Rust client code.

Two independent invariants are enforced:

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

Usage: check_spec_drift.py <openapi.json>
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
REST_RS = os.path.join(REPO, "src", "rest.rs")

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

    if failures:
        sys.exit(1)


if __name__ == "__main__":
    main()
