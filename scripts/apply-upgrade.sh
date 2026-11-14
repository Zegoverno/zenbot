#!/usr/bin/env bash
# Install a build staged by upgrade.sh: wait until no session is working, back up the database,
# swap the binaries, restart and health-check; on failure put the old binaries and database back.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/db.sh"
BIN="$HOME/.zenbot/bin"
LOG="$HOME/.zenbot/upgrade.log"
PORT=$(zen_env ZEN_PORT); PORT=${PORT:-8100}
HEALTH="http://127.0.0.1:$PORT/health"
log() { echo "$(date -u +%FT%TZ) $*" >> "$LOG"; }

STAGE=${1:?usage: apply-upgrade.sh <stage dir from upgrade.sh>}
STAGES=$(dirname "$STAGE")
COMMIT=$(cat "$STAGE/commit" 2>/dev/null)
NOTE=$([ -f "$STAGE/note" ] && echo ", $(cat "$STAGE/note")")
trap 'rm -rf "$STAGE"' EXIT
[ -n "$COMMIT" ] || { log "upgrade ABORTED: no staged build in $STAGE"; exit 1; }
log "upgrade requested ($COMMIT$NOTE)"

# Wait until no session is working. A kernel that doesn't answer has no turn to end; after 30
# minutes a stuck session doesn't block the upgrade any longer.
DEADLINE=$((SECONDS + 1800))
wait_idle() {
  local h fails=0
  while :; do
    if h=$(curl -fs --max-time 10 "$HEALTH"); then
      fails=0
      grep -q '"busy":0' <<<"$h" && return 0
    elif ((++fails >= 5)); then
      log "zenbot isn't answering $HEALTH; going ahead"; return 0
    fi
    if ((SECONDS >= DEADLINE)); then log "still busy after 30 minutes; restarting anyway"; return 0; fi
    sleep 2
  done
}
wait_idle

# One install at a time; the newest request wins.
exec 9>"$STAGES/.lock"
flock 9
NEWEST=$(find "$STAGES" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort -n | tail -1)
if [ "$NEWEST" != "$(basename "$STAGE")" ]; then
  log "upgrade to $COMMIT skipped: a newer request replaces it"
  exit 0
fi

PREV_VERSION=$(cat "$HOME/.zenbot/version" 2>/dev/null || true)
sudo systemctl stop zenbot
BACKUP=$(db_backup "${PREV_VERSION:-unknown}" 2>>"$LOG") || { log "upgrade ABORTED: database backup failed"; sudo systemctl start zenbot; exit 1; }

BINS="zend zen zen-engine"
for b in $BINS; do
  [ ! -f "$BIN/$b" ] || cp -f "$BIN/$b" "$BIN/$b.prev"
  install -m 755 "$STAGE/$b" "$BIN/$b.new" && mv -f "$BIN/$b.new" "$BIN/$b"
done
echo "$COMMIT" > "$HOME/.zenbot/version"
sudo systemctl start zenbot

if wait_healthy "$HEALTH" 45; then
  log "upgrade OK: now running $COMMIT$NOTE"
  "$REPO/scripts/searxng.sh" install >/dev/null 2>&1 || log "SearXNG install failed; web search may be down (scripts/searxng.sh install)"
  SRC=$(zen_env ZEN_REPO); SRC=${SRC:-$HOME/zenbot}
  for t in "$SRC/target" "$SRC/crates/zen-matrix/target"; do
    TRIMMED=$(trim_target "$t" 2>>"$LOG") || true
    [ -z "$TRIMMED" ] || log "$TRIMMED"
  done
  if [ -f "$STAGE/zen-matrix" ]; then
    { [ ! -f "$BIN/zen-matrix" ] || cp -f "$BIN/zen-matrix" "$BIN/zen-matrix.prev"; } \
      && install -m 755 "$STAGE/zen-matrix" "$BIN/zen-matrix.new" && mv -f "$BIN/zen-matrix.new" "$BIN/zen-matrix"
    sudo systemctl restart zen-matrix 2>>"$LOG"; sleep 10
    if systemctl is-active --quiet zen-matrix; then
      cp "$STAGE/matrix-version" "$HOME/.zenbot/matrix.version"
      log "zen-matrix updated"
    else
      [ -f "$BIN/zen-matrix.prev" ] && mv -f "$BIN/zen-matrix.prev" "$BIN/zen-matrix"
      sudo systemctl restart zen-matrix 2>>"$LOG"
      log "zen-matrix update FAILED to start; the previous bridge is back (journalctl -u zen-matrix)"
    fi
  fi
  exit 0
fi

log "upgrade FAILED health check; rolling back. Last service logs:"
journalctl -u zenbot -n 30 --no-pager >> "$LOG" 2>&1
sudo systemctl stop zenbot
for b in $BINS; do [ -f "$BIN/$b.prev" ] && mv -f "$BIN/$b.prev" "$BIN/$b"; done
[ -z "$BACKUP" ] || db_restore "$BACKUP"
echo "$PREV_VERSION" > "$HOME/.zenbot/version"
sudo systemctl start zenbot
if wait_healthy "$HEALTH" 45; then log "rolled back to $PREV_VERSION (binaries and database)"; else log "ROLLBACK ALSO UNHEALTHY: check journalctl -u zenbot"; fi
exit 1
