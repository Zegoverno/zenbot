#!/usr/bin/env bash
# What CI checks, in one command (CI's `check` job runs exactly this): the docs check against the
# base branch (a PROGRESS.md entry included; first, because it takes a second), that no System One
# call bypasses the S1 engine, build once, unit
# tests, clippy with warnings as errors, the binaries' size budgets, and the end-to-end scenarios on
# that build. Prints the time of each step and stops at the first failure.
#
#   scripts/check.sh            every step (before a pull request; CI)
#   scripts/check.sh memory     the fast local path: only the e2e scenarios matching "memory"
#
# crates/zen-matrix has its own CI job and isn't checked here (scripts/matrix.sh, DEVELOPMENT.md).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}  # this VM has 2 CPUs; more has been OOM-killed
BASE=origin/${GITHUB_BASE_REF:-main}            # the pull request's base in CI

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
    summary
    exit $rc
  fi
  SUMMARY+=("$(printf '%-12s ok   %4ss' "$name" "$secs")")
}

# In CI, the step times also go to the job's summary page.
summary() { [ -z "${GITHUB_STEP_SUMMARY:-}" ] || printf '```\n%s\n```\n' "$(printf '%s\n' "${SUMMARY[@]}")" >>"$GITHUB_STEP_SUMMARY"; }

step docs scripts/check-docs.py --base "$BASE"
# Every System One call goes through the S1 engine (crates/zend/src/s1/, D-066).
s1_guard() { ! grep -rnE 's1::wire|decide_with_model|/systemone' crates/zend/src --include='*.rs' | grep -v -e '^crates/zend/src/s1/' -e '^crates/zend/src/config.rs:'; }
step s1-guard s1_guard
step build cargo build --release --locked
step tests cargo test --release --locked
step clippy cargo clippy --release --locked --all-targets -- -D warnings
step sizes scripts/size-budget.sh
step e2e env ZEN_E2E_NO_BUILD=1 scripts/e2e.sh "$@"
SUMMARY+=("all checks passed in $(( $(date +%s) - STARTED ))s")
printf '%s\n' "" "${SUMMARY[@]}" >&2
summary
