#!/usr/bin/env bash
# Build and check zenbot from this checkout, then apply it once no session is working.
# Safe to run from inside a zen session: the restart waits until the current turn ends,
# and a build that doesn't come up healthy is rolled back automatically.
#
#   scripts/upgrade.sh           build, check, smoke test, then schedule the install
#   scripts/upgrade.sh --check   build, check and smoke test only
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
. "$REPO/scripts/db.sh"
CHECK_ONLY=
case ${1:-} in
  --check) CHECK_ONLY=1 ;;
  "") ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

git config core.hooksPath scripts/git-hooks # commit trailers linking zen's commits to sessions

echo "== build"
# Use the binaries CI built for this commit when there are no local code changes;
# otherwise compile here (installing Rust first on machines that never needed it).
PREBUILT=
if ./scripts/fetch-release.sh; then
  PREBUILT=1
  echo "using prebuilt binaries for $(git rev-parse --short HEAD)"
else
  ensure_rust
  rm -f target/release/.prebuilt
  BUILD_LOG=$(mktemp)
  if ! cargo build --release >"$BUILD_LOG" 2>&1; then
    grep -E '^(error|warning)|^\s+-->' -A6 "$BUILD_LOG" | head -80
    echo "BUILD FAILED: fix the errors above, then run this script again."
    exit 1
  fi
  rm -f "$BUILD_LOG"
fi

echo "== check"
if [ -z "$PREBUILT" ]; then # CI already tested prebuilt binaries
  cargo test --release -q 2>&1 | tail -5 || { echo "TESTS FAILED"; exit 1; }
fi
PONG=$(echo '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}' | timeout 15 ./target/release/zen-engine 2>/dev/null | head -1 || true)
echo "$PONG" | grep -q pong || { echo "CHECK FAILED: zen-engine did not answer ping"; exit 1; }
./target/release/zen --version >/dev/null

echo "== smoke"
# Run the new build as a second kernel on a spare port and drive one scripted turn
# (faux engine -> kernel -> bash tool -> answer) through it. It runs on a throwaway copy of
# the live database, so pending migrations are tried there and the live database is
# untouched until the install (which backs it up first if migrations are pending).
SMOKE_PORT=${ZEN_SMOKE_PORT:-18199}
SMOKE_URL="http://127.0.0.1:$SMOKE_PORT"
SMOKE_LOG=$(mktemp)
SMOKE_WS=$(mktemp -d)
SMOKE_DB=zen_smoke_$$
SMOKE_PID=
trap '[ -n "$SMOKE_PID" ] && kill $SMOKE_PID 2>/dev/null; rm -rf "$SMOKE_WS"; db_drop $SMOKE_DB' EXIT
if ! db_copy_live "$SMOKE_DB" >"$SMOKE_LOG" 2>&1; then
  echo "SMOKE TEST FAILED: could not copy the live database ($(db_live_name)) to $SMOKE_DB"
  tail -20 "$SMOKE_LOG"
  exit 1
fi
SMOKE_DB_URL=$(db_url_for "$SMOKE_DB")
(
  set -a; [ -f "$HOME/.zenbot/env" ] && . "$HOME/.zenbot/env"; set +a
  # The smoke kernel runs on a copy of the live database: without these it would start scoring and
  # embedding with the owner's OpenRouter key on every upgrade (real money, nothing tested).
  unset OPENROUTER_API_KEY ZEN_S1_MODEL BRAVE_API_KEY TAVILY_API_KEY
  # ZEN_JOBS=0: the copy's scheduled jobs must not run (engine updates, agent jobs on real models).
  ZEN_JOBS=0 ZEN_TOKEN="$(cat "$HOME/.zenbot/token")" ZEN_PORT=$SMOKE_PORT ZEN_WORKERS=engine ZEN_FAUX=1 ZEN_WORKSPACE="$SMOKE_WS" ZEN_HOME="$SMOKE_WS/.zenbot" \
    ZEN_HARNESS="$(git rev-parse --short HEAD)" DATABASE_URL="$SMOKE_DB_URL" \
    exec ./target/release/zend
) >>"$SMOKE_LOG" 2>&1 &
SMOKE_PID=$!
# Not fatal by itself: a kernel that never comes up fails the turn below, which reports it with the log.
wait_healthy "$SMOKE_URL/health" 30 "$SMOKE_PID" || true
# One scripted turn through zen-engine's faux/smoke.
SMOKE_FAILED=
for SMOKE_MODEL in faux/smoke; do
  RESULT=$(ZEN_URL="$SMOKE_URL" timeout 60 ./target/release/zen ask --json -m "$SMOKE_MODEL" "upgrade smoke test" 2>/dev/null || true)
  if ! echo "$RESULT" | jq -e '.error == null and (.text | contains("Smoke test passed")) and .tools[0].is_error == false' >/dev/null 2>&1; then
    SMOKE_FAILED="$SMOKE_MODEL: ${RESULT:-none}"
    break
  fi
  echo "turn on $SMOKE_MODEL ok"
done
kill $SMOKE_PID 2>/dev/null; wait $SMOKE_PID 2>/dev/null || true
SMOKE_PID=
if [ -n "$SMOKE_FAILED" ]; then
  echo "SMOKE TEST FAILED: the new build could not run a turn on $SMOKE_FAILED"
  tail -20 "$SMOKE_LOG"
  exit 1
fi
rm -f "$SMOKE_LOG"
PENDING=$(db_pending | tr '\n' ' ')
[ -n "$PENDING" ] && echo "migrations pending on the live database: $PENDING(tested on a copy; the install backs it up first)"

if [ -n "$CHECK_ONLY" ]; then
  echo "Check OK ($(git rev-parse --short HEAD 2>/dev/null)$(git diff --quiet 2>/dev/null || echo ', uncommitted changes')); nothing installed."
  exit 0
fi

echo "== schedule"
sudo systemd-run --quiet --collect --unit "zen-upgrade-$(date +%s)" --uid "$(id -u)" --gid "$(id -g)" \
  --setenv=HOME="$HOME" --setenv=PATH="$PATH" "$REPO/scripts/apply-upgrade.sh"
echo "Build OK ($(git rev-parse --short HEAD 2>/dev/null)$(git diff --quiet 2>/dev/null || echo ', uncommitted changes'))."
echo "The new version will be installed and zenbot restarted as soon as no session is working."
echo "Result is logged to ~/.zenbot/upgrade.log (rolls back automatically if unhealthy)."
