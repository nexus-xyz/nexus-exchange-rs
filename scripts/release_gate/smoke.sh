#!/usr/bin/env bash
# Pre-publish smoke test (ENG-18798). Packs the crate as a publish would, builds a fresh binary crate
# whose one SDK dependency is the unpacked .crate, and runs one unauthenticated read against the
# public testnet with it (scripts/release_gate/smoke/main.rs). No keys, no writes.
#
#   scripts/release_gate/smoke.sh               pack, build, read
#   scripts/release_gate/smoke.sh --build-only  pack and build, no network (PRs that are not a release)
#
# Exit codes, kept apart on purpose: 0 passed, 1 failed, 2 testnet unreachable. The workflow fails
# on 1 and on 2, under different names. Unreachable is not a pass, and it is not the SDK's fault:
# re-run the job once testnet answers.
set -euo pipefail

mode="read"
case "${1:-}" in
  "") ;;
  --build-only) mode="build" ;;
  *) echo "usage: $0 [--build-only]" >&2; exit 64 ;;
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

cargo package --locked --allow-dirty --no-verify --quiet
tar -xzf "${target_dir}/package/${name}-${version}.crate" -C "$work"

mkdir -p "$work/smoke/src"
cp scripts/release_gate/smoke/main.rs "$work/smoke/src/main.rs"
cat > "$work/smoke/Cargo.toml" <<EOF
[package]
name = "nexus-exchange-smoke"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
${name} = { path = "../${name}-${version}" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

# A workspace of its own, so cargo never adopts it into one above the temp dir.
[workspace]
EOF
# Resolve against the versions the packed crate ships in its own Cargo.lock.
cp "$work/${name}-${version}/Cargo.lock" "$work/smoke/Cargo.lock"
CARGO_TARGET_DIR="$target_dir" cargo build --quiet --manifest-path "$work/smoke/Cargo.toml"
echo "built a consumer of ${name}-${version}.crate"

summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '### Testnet smoke: %s\n\n%s\n' "$1" "$2" >> "$GITHUB_STEP_SUMMARY"
  fi
}

if [ "$mode" = "build" ]; then
  echo "::notice title=prepublish-smoke (read not attempted)::Not a release PR: the crate packed and a clean consumer built, and no testnet read was made. The read runs on the release PR."
  summary "build only" "Not a release PR: \`${name}-${version}.crate\` packed and a clean consumer built. No testnet read was attempted, so this is not a smoke pass."
  exit 0
fi

set +e
line="$("${target_dir}/debug/nexus-exchange-smoke")"
code=$?
set -e
echo "$line"
case "$code" in
  0)
    summary "✅ passed" "$line"
    ;;
  2)
    echo "::error title=prepublish-smoke (testnet unreachable)::NOT a pass: the testnet read got no usable answer, so nothing about this release was verified. Re-run this job once testnet answers. ${line}"
    summary "⚠️ TESTNET UNREACHABLE: not a pass" "$line"
    ;;
  *)
    echo "::error title=prepublish-smoke (failed)::The packed crate could not make an unauthenticated testnet read. ${line}"
    summary "❌ FAILED" "$line"
    code=1
    ;;
esac
exit "$code"
