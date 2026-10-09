#!/usr/bin/env bash
# Public-surface snapshot (ENG-18798). Lists the public API of the crate exactly as
# `cargo package` packs it (the .crate a publish uploads, unpacked) and compares it with the
# committed `public-api.txt`.
#
#   scripts/release_gate/public_api.sh           check: fail on any difference (CI, `prepublish-surface`)
#   scripts/release_gate/public_api.sh --write   regenerate public-api.txt after a deliberate API change
#
# Any difference fails, additions included, so the snapshot moves in the same PR as the code. A
# removed or changed item then shows up as a `-` line in the diff a reviewer reads, instead of
# first surfacing in the release PR's cargo-semver-checks report (or in a downstream build).
#
# What it does not see. `-ss` leaves auto-trait impls out of the listing, so a type that stops being
# Send, Sync or UnwindSafe (a new field can do that) passes here. cargo-public-api does not render
# private fields either, so adding one to an exhaustive struct whose fields are all public, which
# breaks struct-literal construction downstream, passes too. The `semver` job (cargo-semver-checks,
# lints `auto_trait_impl_removed` and `constructible_struct_adds_private_field`) catches both when
# the PR does not declare the break.
#
# Needs cargo-public-api and a nightly that it can read, because the listing comes from rustdoc
# JSON, which is nightly-only. Both are pinned, and they move together: cargo-public-api 0.52.x
# reads the rustdoc JSON of nightly-2025-11-22 onward, until a nightly changes the format again.
# When that happens this script fails with a format error rather than a diff. Bump both, in the
# workflow and here, and regenerate. It is the same upkeep the `semver` job's pinned
# cargo-semver-checks needs (ENG-11844).
set -euo pipefail

NIGHTLY="${PUBLIC_API_NIGHTLY:-nightly-2026-10-01}"
SNAPSHOT="public-api.txt"

mode="check"
case "${1:-}" in
  "") ;;
  --write) mode="write" ;;
  *) echo "usage: $0 [--write]" >&2; exit 2 ;;
esac

root="$(git rev-parse --show-toplevel)"
cd "$root"

# The target directory too, not a hardcoded ./target: `cargo package` writes into
# CARGO_TARGET_DIR (or build.target-dir) when one is set.
meta="$(cargo metadata --no-deps --format-version 1)"
name="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["name"])' <<<"$meta")"
version="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])' <<<"$meta")"
target_dir="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<<"$meta")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# What a publish would upload. --allow-dirty so `--write` works on uncommitted edits; in CI the
# tree is the PR head and clean anyway. --no-verify: the listing below builds the docs from the
# unpacked crate, which is the verification that matters here.
cargo package --locked --allow-dirty --no-verify --quiet
tar -xzf "${target_dir}/package/${name}-${version}.crate" -C "$work"

CARGO_TARGET_DIR="$target_dir" cargo "+${NIGHTLY}" public-api -ss \
  --manifest-path "$work/${name}-${version}/Cargo.toml" > "$work/public-api.txt"

if [ "$mode" = "write" ]; then
  cp "$work/public-api.txt" "$SNAPSHOT"
  echo "wrote $SNAPSHOT ($(wc -l < "$SNAPSHOT") items) from ${name}-${version}.crate"
  exit 0
fi

if diff -u --label "$SNAPSHOT (committed)" --label "$SNAPSHOT (built ${name}-${version}.crate)" \
  "$SNAPSHOT" "$work/public-api.txt" > "$work/diff.txt"; then
  echo "public surface matches $SNAPSHOT ($(wc -l < "$SNAPSHOT") items)"
  exit 0
fi

removed="$(grep -c '^-[^-]' "$work/diff.txt" || true)"
added="$(grep -c '^+[^+]' "$work/diff.txt" || true)"
cat "$work/diff.txt"
echo "::error title=prepublish-surface::The built crate's public API differs from $SNAPSHOT: ${removed} item(s) gone or changed, ${added} new. If that is deliberate, run scripts/release_gate/public_api.sh --write and commit $SNAPSHOT in this PR, so the change is in the diff a reviewer reads. A removal or change is breaking."
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Public surface: ❌ differs from \`$SNAPSHOT\`"
    echo
    echo "${removed} item(s) gone or changed (\`-\`), ${added} new (\`+\`). Regenerate with \`scripts/release_gate/public_api.sh --write\` if deliberate."
    echo
    echo '```diff'
    cat "$work/diff.txt"
    echo '```'
  } >> "$GITHUB_STEP_SUMMARY"
fi
exit 1
