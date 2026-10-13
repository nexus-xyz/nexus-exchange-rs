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
# Exits 2 and 1 are exercised on every PR, not only asserted. In both modes, right after the build
# and before any testnet read, the just-built smoke runs three times with NEXUS_SMOKE_BASE_URL
# overridden: at http://127.0.0.1:1, where nothing listens, it must exit 2; at stub_server.py 503,
# which answers every request with a 503 like an outage, it must exit 2; at stub_server.py, which
# answers every request with an empty list, it must exit 1. Exit 0 is exercised only by the real
# read, so only on a release PR or a change to this gate. Each probe runs under a 180 s timeout
# (`timeout`, or `gtimeout`; with neither, as on a stock Mac, without a limit, and it says so), and
# a wrong code or a timeout fails this script with a `prepublish-smoke (outcomes)` error naming the
# case. The probes run the binary directly with its stdout and stderr captured, so none of the
# annotations below for a real 1 or 2 are printed for them: each logs one plain line instead.
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
name="$(python3 -I -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["name"])' <<<"$meta")"
version="$(python3 -I -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])' <<<"$meta")"
target_dir="$(python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<<"$meta")"

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

# 180 s, not less: the SDK retries transient errors with backoff before it gives up. `timeout` is GNU
# coreutils; a Mac may have it as `gtimeout`, or not at all, and then the probes run unbounded.
if command -v timeout > /dev/null; then
  probe_timeout=(timeout 180)
elif command -v gtimeout > /dev/null; then
  probe_timeout=(gtimeout 180)
else
  probe_timeout=()
  echo "note: neither timeout nor gtimeout is installed, so the outcome probes run without a time limit"
fi

# The outcome probes (header). $1 names the case, $2 is the base URL, $3 the exit code it must give.
probe() {
  local got first
  set +e
  # The `+` form, because bash 3.2 (a Mac's) treats an empty array as unset under `set -u`.
  NEXUS_SMOKE_BASE_URL="$2" ${probe_timeout[@]+"${probe_timeout[@]}"} "$smoke" > "$work/probe.out" 2>&1
  got=$?
  set -e
  first="$(head -n 1 "$work/probe.out")"
  echo "outcome probe, $1: exit ${got}: ${first}"
  if [ "$got" = 124 ]; then
    echo "::error title=prepublish-smoke (outcomes)::$1: the smoke did not finish within 180 s, so its exit codes can't be trusted."
    exit 1
  fi
  if [ "$got" != "$3" ]; then
    echo "::error title=prepublish-smoke (outcomes)::$1: the smoke should exit $3 and exited ${got}, so its exit codes can't be trusted. ${first}"
    exit 1
  fi
}

# Starts stub_server.py with the given arguments (none, or a status) and sets stub_pid and port.
start_stub() {
  # Created first, so the wait below never reads a file the stub has not opened yet.
  : > "$work/stub.out"
  python3 -I scripts/release_gate/stub_server.py "$@" > "$work/stub.out" &
  stub_pid=$!
  # The stub prints its port as its first line; wait for that line, or for the stub to die.
  for _ in $(seq 100); do
    if [ "$(wc -l < "$work/stub.out")" -ge 1 ] || ! kill -0 "$stub_pid" 2> /dev/null; then break; fi
    sleep 0.1
  done
  port="$(head -n 1 "$work/stub.out")"
  if ! [[ "$port" =~ ^[0-9]+$ ]]; then
    echo "::error title=prepublish-smoke (outcomes)::stub_server.py $* did not print a port (got '${port}'), so its case could not run."
    exit 1
  fi
}

stop_stub() {
  kill "$stub_pid" 2> /dev/null || true
  wait "$stub_pid" 2> /dev/null || true
  stub_pid=""
}

probe "nothing listening (http://127.0.0.1:1)" "http://127.0.0.1:1" 2

start_stub 503
probe "a stub that answers 503 (http://127.0.0.1:${port})" "http://127.0.0.1:${port}" 2
stop_stub

start_stub
probe "a stub that answers with no markets (http://127.0.0.1:${port})" "http://127.0.0.1:${port}" 1
stop_stub

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
