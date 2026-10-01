#!/usr/bin/env bash
# Runs detached (via systemd-run) from upgrade.sh: waits for idle, swaps binaries,
# restarts, health-checks, and rolls back on failure.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$HOME/.zenbot/bin"
LOG="$HOME/.zenbot/upgrade.log"
PORT=$(grep -E '^ZEN_PORT=' "$HOME/.zenbot/env" 2>/dev/null | cut -d= -f2); PORT=${PORT:-8100}
HEALTH="http://127.0.0.1:$PORT/health"
log() { echo "$(date -u +%FT%TZ) $*" >> "$LOG"; }

healthy() { curl -fs "$HEALTH" | grep -q '"ok":true'; }
wait_healthy() { for _ in $(seq 1 45); do healthy && return 0; sleep 1; done; return 1; }

log "upgrade requested ($(git -C "$REPO" rev-parse --short HEAD 2>/dev/null))"
for _ in $(seq 1 900); do
  curl -fs "$HEALTH" | grep -q '"busy":0' && break
  sleep 2
done

BINS="zend zen zen-engine"
for b in $BINS; do
  [ -f "$BIN/$b" ] && cp -f "$BIN/$b" "$BIN/$b.prev"
  install -m 755 "$REPO/target/release/$b" "$BIN/$b.new" && mv -f "$BIN/$b.new" "$BIN/$b"
done
sudo systemctl restart zenbot

if wait_healthy; then
  git -C "$REPO" rev-parse --short HEAD > "$HOME/.zenbot/version" 2>/dev/null
  log "upgrade OK: now running $(cat "$HOME/.zenbot/version")"
  exit 0
fi

log "upgrade FAILED health check; rolling back. Last service logs:"
journalctl -u zenbot -n 30 --no-pager >> "$LOG" 2>&1
for b in $BINS; do [ -f "$BIN/$b.prev" ] && mv -f "$BIN/$b.prev" "$BIN/$b"; done
sudo systemctl restart zenbot
if wait_healthy; then log "rolled back to previous version"; else log "ROLLBACK ALSO UNHEALTHY: check journalctl -u zenbot"; fi
exit 1
