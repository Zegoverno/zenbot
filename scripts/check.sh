#!/usr/bin/env bash
# What CI checks, in one command: the web app's types, unit tests and browser end-to-end tests
# (web/, Node 22 and Playwright's Chromium), then the build, unit tests, clippy with warnings as
# errors, and the kernel's end-to-end scenarios (scripts/e2e.sh) on that build. Prints each step's
# time and stops at the first failure.
#
#   scripts/check.sh            every step
#   scripts/check.sh memory     the web checks, then only the e2e scenarios matching "memory"
#
# crates/zen-matrix is its own Cargo workspace and isn't checked here (scripts/matrix.sh --build).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}  # this VM has 2 CPUs; more has been OOM-killed

SUMMARY=()
STARTED=$(date +%s)
step() { # name command…
  local name=$1 t0 rc=0; shift
  echo "== $name" >&2
  t0=$(date +%s)
  "$@" || rc=$?
  local secs=$(( $(date +%s) - t0 ))
  if [ $rc -ne 0 ]; then
    printf '%s\n' "" "${SUMMARY[@]}" "$(printf '%-8s FAILED after %ss' "$name" "$secs")" >&2
    exit $rc
  fi
  SUMMARY+=("$(printf '%-8s ok   %4ss' "$name" "$secs")")
}

web() { (cd web && "$@"); }
step web web sh -c 'npm ci --no-audit --no-fund --loglevel=error && npm run check --silent'
step web-e2e web sh -c 'node tests/e2e.mjs && node tests/zen-e2e.mjs'
step build cargo build --release --locked
. "$REPO/scripts/test-sqlite.sh"
step sqlite ensure_test_sqlite
step tests cargo test --release --locked
step clippy cargo clippy --release --locked --all-targets -- -D warnings
step e2e env ZEN_E2E_NO_BUILD=1 scripts/e2e.sh "$@"
printf '%s\n' "" "${SUMMARY[@]}" "all checks passed in $(( $(date +%s) - STARTED ))s" >&2
