#!/usr/bin/env python3
"""Check the SDK's targeted endpoints against the pinned OpenAPI spec.

Fails if any endpoint the SDK targets (endpoints.txt) is missing from the spec
(a breaking change, rename, or typo). Reports spec operations the SDK does not
yet cover as an informational coverage gap.

Usage: check_spec_drift.py <openapi.json>
"""
import json
import sys


def load_targeted(path="endpoints.txt"):
    out = []
    for line in open(path):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        method, p = line.split(None, 1)
        out.append((method.upper(), p))
    return out


def spec_ops(spec):
    ops = set()
    for p, methods in spec.get("paths", {}).items():
        for m in methods:
            if m.lower() in ("get", "post", "put", "delete", "patch"):
                ops.add((m.upper(), p))
    return ops


def main():
    spec = json.load(open(sys.argv[1]))
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

    if missing:
        print(f"\nERROR: {len(missing)} targeted endpoint(s) are NOT in the spec "
              f"(removed/renamed/typo):")
        for m, p in missing:
            print(f"  - {m} {p}")
        sys.exit(1)

    print("\nOK: every targeted endpoint exists in the pinned spec.")


if __name__ == "__main__":
    main()
