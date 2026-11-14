#!/usr/bin/env bash
# Build zenbot from this checkout (whatever is checked out, branch or main) and install it once no
# session is working. Safe to run from inside a zen session: the restart waits for the turn to end,
# and apply-upgrade.sh rolls back if the new kernel isn't healthy.
#
#   scripts/upgrade.sh           build, then schedule the install
#   scripts/upgrade.sh --check   build only
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
. "$REPO/scripts/lib.sh"
CHECK_ONLY=
case ${1:-} in
  --check) CHECK_ONLY=1 ;;
  "") ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

git config core.hooksPath scripts/git-hooks # commit trailers linking zen's commits to sessions
COMMIT=$(git rev-parse --short HEAD)
DIRTY=$(git diff --quiet HEAD 2>/dev/null || echo ", uncommitted changes")

echo "== build ($COMMIT$DIRTY)"
ensure_rust
BUILD_LOG=$(mktemp)
if ! nice -n 10 cargo build --profile dist >"$BUILD_LOG" 2>&1; then
  grep -E '^(error|warning)|^\s+-->' -A6 "$BUILD_LOG" | head -80
  echo "BUILD FAILED: fix the errors above, then run this script again."
  exit 1
fi
rm -f "$BUILD_LOG"
PONG=$(echo '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}' | timeout 15 ./target/dist/zen-engine 2>/dev/null | head -1 || true)
echo "$PONG" | grep -q pong || { echo "CHECK FAILED: zen-engine did not answer ping"; exit 1; }
./target/dist/zen --version >/dev/null

# zen-matrix, when installed and enabled, is rebuilt if its sources (or the crates it shares) changed.
matrix_src=$(git log -1 --format=%h -- crates/zen-matrix crates/zen-proto crates/zen-client 2>/dev/null)
matrix_bin=
if [ -z "$CHECK_ONLY" ] && [ -n "$matrix_src" ] && systemctl is-enabled --quiet zen-matrix 2>/dev/null \
  && [ "$(cat "$HOME/.zenbot/matrix.version" 2>/dev/null)" != "$matrix_src" ]; then
  echo "== zen-matrix (sources changed)"
  if (cd crates/zen-matrix && cargo build --release --locked -q); then
    matrix_bin=crates/zen-matrix/target/release/zen-matrix
  else
    echo "zen-matrix didn't build; the installed bridge stays as it is"
  fi
fi

if [ -n "$CHECK_ONLY" ]; then
  echo "Build OK ($COMMIT$DIRTY); nothing installed."
  exit 0
fi

echo "== schedule"
# Stage the built binaries: the install runs later (once no session is working), by which time this
# checkout may have moved on.
STAGE="$HOME/.zenbot/upgrades/$(date +%s%N)"
mkdir -p "$STAGE"
find "$HOME/.zenbot/upgrades" -mindepth 1 -maxdepth 1 -type d -mmin +120 -exec rm -rf {} +
for b in zend zen zen-engine; do cp "target/dist/$b" "$STAGE/$b"; done
[ -z "$matrix_bin" ] || { cp "$matrix_bin" "$STAGE/zen-matrix" && echo "$matrix_src" > "$STAGE/matrix-version"; }
echo "$COMMIT" > "$STAGE/commit"
[ -z "$DIRTY" ] || echo "uncommitted changes" > "$STAGE/note"
sudo systemd-run --quiet --collect --unit "zen-upgrade-$(basename "$STAGE")" --uid "$(id -u)" --gid "$(id -g)" \
  --setenv=HOME="$HOME" --setenv=PATH="$PATH" "$REPO/scripts/apply-upgrade.sh" "$STAGE"
echo "Build OK ($COMMIT$DIRTY). It installs and zenbot restarts as soon as no session is working."
echo "Result: ~/.zenbot/upgrade.log"
