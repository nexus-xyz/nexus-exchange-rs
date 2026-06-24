#!/usr/bin/env python3
"""Update the README SDK<->spec compatibility table for a new pinned spec tag.

`sync_api_version.py` owns the bot-managed "currently targets" line and
`.api-version`. This script owns one more thing the auto-bump pipeline needs
(ENG-3563): the API-spec cell of the **top row** of the compatibility table.

The table records which SDK minor series targets which spec, newest row first:

    | SDK version | API spec |
    |---|---|
    | `0.3.x` | `v0.4.0` |   <- top row = the current in-development SDK series
    | `0.1.x`–`0.2.x` | `v0.3.5` |

The top row always describes the SDK series under active development, so a spec
bump just advances *its* API-spec cell to the new tag. The historical rows below
record what shipped SDK versions targeted and are never touched — a bare spec
release doesn't change history; the next SDK release (release-plz) appends a new
top row when it cuts a version that ships the bump.

This keeps the table honest for a NON-BREAKING auto-merge: the in-development row
simply advances its pinned spec. A breaking bump goes to a human, who may adjust
the SDK version / split the row as part of planning the SDK update.

Usage:
  update_compat_table.py --tag vX.Y.Z [--readme README.md]
"""
import argparse
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

TAG_RE = re.compile(r"^v[0-9]+(\.[0-9]+){0,2}$")
# A table row: `| <SDK version cell> | `<spec>` |`. The SDK-version cell can hold
# ranges/multiple code spans (e.g. `0.1.x`–`0.2.x`), so only the API-spec cell is
# constrained to a single `vX.Y.Z` code span.
ROW_RE = re.compile(r"^\|\s*(?P<sdk>.+?)\s*\|\s*`(?P<spec>v[0-9][^`]*)`\s*\|\s*$")
HEADER_RE = re.compile(r"^\|\s*SDK version\s*\|\s*API spec\s*\|\s*$")


def fail(msg):
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--tag", required=True, metavar="vX.Y.Z")
    ap.add_argument("--readme", default=os.path.join(REPO, "README.md"))
    args = ap.parse_args()

    if not TAG_RE.match(args.tag):
        fail(f"--tag is not a valid version tag (want vX.Y.Z): {args.tag!r}")

    try:
        with open(args.readme) as f:
            lines = f.read().splitlines(keepends=True)
    except OSError as e:
        fail(f"cannot read {args.readme}: {e}")

    # Find the table header + separator; the first data row follows.
    header_idx = next((i for i, ln in enumerate(lines) if HEADER_RE.match(ln)), None)
    if header_idx is None:
        fail("compatibility table header '| SDK version | API spec |' not found")
    first_row_idx = header_idx + 2  # header, separator, then rows
    if first_row_idx >= len(lines):
        fail("expected at least one data row under the compatibility table")

    m = ROW_RE.match(lines[first_row_idx])
    if not m:
        fail(
            "the first compatibility-table row does not match "
            "`| <sdk> | `vX.Y.Z` |`: " + lines[first_row_idx].rstrip()
        )

    sdk_cell, old_spec = m.group("sdk"), m.group("spec")
    if old_spec == args.tag:
        print(f"Compat table top row already targets {args.tag}; no change.")
        return

    nl = "\n" if lines[first_row_idx].endswith("\n") else ""
    lines[first_row_idx] = f"| {sdk_cell} | `{args.tag}` |{nl}"
    print(f"Updated compat top row ({sdk_cell}): {old_spec} -> {args.tag}.")

    with open(args.readme, "w") as f:
        f.write("".join(lines))


if __name__ == "__main__":
    main()
