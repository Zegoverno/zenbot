#!/usr/bin/env bash
# Build and check zenbot from this checkout, then apply it once no session is working.
# Safe to run from inside a zen session: the restart waits until the current turn ends,
# and a build that doesn't come up healthy is rolled back automatically.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"

echo "== build"
if grep -qE '^ZEN_WORKERS=.*pi' "$HOME/.zenbot/env" 2>/dev/null; then (cd packages/mind && npm ci --no-audit --no-fund --silent); fi
BUILD_LOG=$(mktemp)
if ! cargo build --release >"$BUILD_LOG" 2>&1; then
  grep -E '^(error|warning)|^\s+-->' -A6 "$BUILD_LOG" | head -80
  echo "BUILD FAILED: fix the errors above, then run this script again."
  exit 1
fi
rm -f "$BUILD_LOG"

echo "== check"
cargo test --release -q 2>&1 | tail -5 || { echo "TESTS FAILED"; exit 1; }
PONG=$(echo '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}' | timeout 15 ./target/release/zen-engine 2>/dev/null | head -1 || true)
echo "$PONG" | grep -q pong || { echo "CHECK FAILED: zen-engine did not answer ping"; exit 1; }
if grep -qE '^ZEN_WORKERS=.*pi' "$HOME/.zenbot/env" 2>/dev/null; then
  PONG=$(echo '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}' | timeout 15 node packages/mind/src/main.ts 2>/dev/null | head -1 || true)
  echo "$PONG" | grep -q pong || { echo "CHECK FAILED: zen-mind (pi) did not answer ping"; exit 1; }
fi
./target/release/zen --version >/dev/null

echo "== schedule"
sudo systemd-run --quiet --collect --unit "zen-upgrade-$(date +%s)" --uid "$(id -u)" --gid "$(id -g)" \
  --setenv=HOME="$HOME" --setenv=PATH="$PATH" "$REPO/scripts/apply-upgrade.sh"
echo "Build OK ($(git rev-parse --short HEAD 2>/dev/null)$(git diff --quiet 2>/dev/null || echo ', uncommitted changes'))."
echo "The new version will be installed and zenbot restarted as soon as no session is working."
echo "Result is logged to ~/.zenbot/upgrade.log (rolls back automatically if unhealthy)."
