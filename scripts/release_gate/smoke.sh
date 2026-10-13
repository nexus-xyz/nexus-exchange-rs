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
#
# Those outcomes are exercised, not only asserted. In both modes, right after the build and before
# any testnet read, the just-built smoke runs twice with NEXUS_SMOKE_BASE_URL overridden: at
# http://127.0.0.1:1, where nothing listens, it must exit 2; at stub_server.py, which answers every
# request with an empty list, it must exit 1. Any other code fails this script with a
# `prepublish-smoke (outcomes)` error naming the case. The probes run the binary directly, so the
# annotations below for a real 1 or 2 are not printed for them.
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
stub_pid=""
cleanup() {
  if [ -n "$stub_pid" ]; then kill "$stub_pid" 2> /dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

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
smoke="${target_dir}/debug/nexus-exchange-smoke"

# The outcome probes (header). $1 names the case, $2 is the base URL, $3 the exit code it must give.
probe() {
  local out got
  set +e
  out="$(NEXUS_SMOKE_BASE_URL="$2" "$smoke")"
  got=$?
  set -e
  echo "outcome probe, $1: exit ${got}: ${out}"
  if [ "$got" != "$3" ]; then
    echo "::error title=prepublish-smoke (outcomes)::$1: the smoke should exit $3 and exited ${got}, so its exit codes can't be trusted. ${out}"
    exit 1
  fi
}

probe "nothing listening (http://127.0.0.1:1)" "http://127.0.0.1:1" 2

python3 -I scripts/release_gate/stub_server.py > "$work/stub.out" &
stub_pid=$!
# The stub prints its port as its first line; wait for that line, or for the stub to die.
for _ in $(seq 100); do
  if [ "$(wc -l < "$work/stub.out")" -ge 1 ] || ! kill -0 "$stub_pid" 2> /dev/null; then break; fi
  sleep 0.1
done
port="$(head -n 1 "$work/stub.out")"
if ! [[ "$port" =~ ^[0-9]+$ ]]; then
  echo "::error title=prepublish-smoke (outcomes)::stub_server.py did not print a port (got '${port}'), so the exit-1 case could not run."
  exit 1
fi
probe "a stub that answers with no markets (http://127.0.0.1:${port})" "http://127.0.0.1:${port}" 1
kill "$stub_pid" 2> /dev/null || true
stub_pid=""

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
line="$("$smoke")"
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
