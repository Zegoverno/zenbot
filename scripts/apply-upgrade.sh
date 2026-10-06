#!/usr/bin/env bash
# Runs detached (via systemd-run) from upgrade.sh: waits for idle, swaps binaries,
# restarts, health-checks, and rolls back on failure. If the new build brings migrations the
# live database hasn't applied, the database is backed up first (scripts/db.sh); a rollback
# doesn't restore it (migrations are expand-only, see AGENTS.md) but logs how to.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/db.sh"
BIN="$HOME/.zenbot/bin"
LOG="$HOME/.zenbot/upgrade.log"
PORT=$(grep -E '^ZEN_PORT=' "$HOME/.zenbot/env" 2>/dev/null | cut -d= -f2); PORT=${PORT:-8100}
HEALTH="http://127.0.0.1:$PORT/health"
log() { echo "$(date -u +%FT%TZ) $*" >> "$LOG"; }

healthy() { curl -fs "$HEALTH" | grep -q '"ok":true'; }
wait_healthy() { for _ in $(seq 1 45); do healthy && return 0; sleep 1; done; return 1; }

log "upgrade requested ($(git -C "$REPO" rev-parse --short HEAD 2>/dev/null))"
IDLE=
for _ in $(seq 1 900); do
  curl -fs "$HEALTH" | grep -q '"busy":0' && { IDLE=1; break; }
  sleep 2
done
# After 30 minutes it goes ahead anyway (a stuck session must not block upgrades forever), but says so.
[ -n "$IDLE" ] || log "zenbot still busy (or not answering) after 30 minutes; restarting anyway, which ends the running turns"

PREV_VERSION=$(cat "$HOME/.zenbot/version" 2>/dev/null || true)
BACKUP=
PENDING=$(db_pending | tr '\n' ' ')
if [ -n "$PENDING" ]; then
  if ! BACKUP=$(db_backup "${PREV_VERSION:-unknown}" 2>>"$LOG"); then
    log "upgrade ABORTED: migrations ${PENDING}pending but the database backup failed; nothing changed"
    exit 1
  fi
  log "migrations ${PENDING}pending; database backed up to $BACKUP"
fi

BINS="zend zen zen-engine"
for b in $BINS; do
  [ -f "$BIN/$b" ] && cp -f "$BIN/$b" "$BIN/$b.prev"
  install -m 755 "$REPO/target/release/$b" "$BIN/$b.new" && mv -f "$BIN/$b.new" "$BIN/$b"
done
# The kernel reads the version at start and records it with every turn, so write it first.
git -C "$REPO" rev-parse --short HEAD > "$HOME/.zenbot/version" 2>/dev/null
sudo systemctl restart zenbot

if wait_healthy; then
  log "upgrade OK: now running $(cat "$HOME/.zenbot/version")"
  exit 0
fi

log "upgrade FAILED health check; rolling back. Last service logs:"
journalctl -u zenbot -n 30 --no-pager >> "$LOG" 2>&1
for b in $BINS; do [ -f "$BIN/$b.prev" ] && mv -f "$BIN/$b.prev" "$BIN/$b"; done
echo "$PREV_VERSION" > "$HOME/.zenbot/version"
sudo systemctl restart zenbot
if wait_healthy; then log "rolled back to previous version"; else log "ROLLBACK ALSO UNHEALTHY: check journalctl -u zenbot"; fi
if [ -n "$BACKUP" ]; then
  log "the new build may have applied migrations ${PENDING}(not undone). To restore the database as it was before: $(db_restore_cmd "$BACKUP")"
fi
exit 1
