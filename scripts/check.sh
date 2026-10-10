#!/usr/bin/env bash
# What CI checks, in one command, before a pull request: build once, unit tests, clippy with
# warnings as errors, the end-to-end scenarios on that build, and the docs check against
# origin/main (a PROGRESS.md entry included). Prints the time of each step and stops at the first
# failure.
#
#   scripts/check.sh            every step
#   scripts/check.sh memory     ... with only the e2e scenarios whose name contains "memory"
#
# crates/zen-matrix has its own CI job and isn't checked here (scripts/matrix.sh, DEVELOPMENT.md).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.cargo/bin:$PATH"
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
    SUMMARY+=("$(printf '%-12s FAILED after %ss' "$name" "$secs")")
    printf '%s\n' "" "${SUMMARY[@]}" "" "stopped: $name failed (exit $rc); fix it and run scripts/check.sh again" >&2
    exit $rc
  fi
  SUMMARY+=("$(printf '%-12s ok   %4ss' "$name" "$secs")")
}

step build cargo build --release --locked
step tests cargo test --release --locked
step clippy cargo clippy --release --locked --all-targets -- -D warnings
step e2e env ZEN_E2E_NO_BUILD=1 scripts/e2e.sh "$@"
step docs scripts/check-docs.py --base origin/main
printf '%s\n' "" "${SUMMARY[@]}" "all checks passed in $(( $(date +%s) - STARTED ))s" >&2
