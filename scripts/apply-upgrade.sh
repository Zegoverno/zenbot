#!/usr/bin/env bash
# Runs detached (via systemd-run) from upgrade.sh with the stage it made (~/.zenbot/upgrades/<id>:
# the tested binaries, `commit`, and `note` if the tree had uncommitted changes): waits for idle,
# swaps in the staged binaries, restarts, health-checks, and rolls back on failure. Installs are
# serialized by a lock, and a request superseded by a newer one steps aside instead of installing
# over it, so the log and ~/.zenbot/version always name the commit actually running. If the new build brings migrations the
# live database hasn't applied, the database is backed up first (scripts/db.sh); a rollback
# doesn't restore it (migrations are expand-only, see AGENTS.md) but logs how to.
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
if [ -z "$COMMIT" ]; then log "upgrade ABORTED: no staged build in $STAGE; nothing changed"; exit 1; fi
trap 'rm -rf "$STAGE"' EXIT

log "upgrade requested ($COMMIT$NOTE)"
IDLE=
for _ in $(seq 1 900); do
  curl -fs "$HEALTH" | grep -q '"busy":0' && { IDLE=1; break; }
  sleep 2
done
# After 30 minutes it goes ahead anyway (a stuck session must not block upgrades forever), but says so.
[ -n "$IDLE" ] || log "zenbot still busy (or not answering) after 30 minutes; restarting anyway, which ends the running turns"

# One install at a time; the newest request wins (stage ids are nanosecond timestamps, so they sort).
exec 9>"$STAGES/.lock"
flock 9
# A newer request still waiting, or one already installed (its id is in .installed), wins.
ID=$(basename "$STAGE")
NEWEST=$(find "$STAGES" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort -n | tail -1)
INSTALLED=$(cat "$STAGES/.installed" 2>/dev/null || echo 0)
if [ "$NEWEST" != "$ID" ]; then
  log "upgrade to $COMMIT skipped: a newer request ($(cat "$STAGES/$NEWEST/commit" 2>/dev/null)) replaces it"
  exit 0
elif [ "$INSTALLED" -ge "$ID" ] 2>/dev/null; then
  log "upgrade to $COMMIT skipped: a newer request was already installed"
  exit 0
fi
echo "$ID" > "$STAGES/.installed"

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
  install -m 755 "$STAGE/$b" "$BIN/$b.new" && mv -f "$BIN/$b.new" "$BIN/$b"
done
# The kernel reads the version at start and records it with every turn, so write it first.
echo "$COMMIT" > "$HOME/.zenbot/version"
sudo systemctl restart zenbot

if wait_healthy "$HEALTH" 45; then
  log "upgrade OK: now running $COMMIT$NOTE"
  # The kernel's scheduler runs the sleep and engine updates now (D-046): the old timers go.
  remove_old_timers 2>>"$LOG" || log "removing the old zen-sleep/zen-engines timers failed; remove them by hand"
  # Services a new version brings in deploy/compose.yaml (e.g. SearXNG for web search).
  sudo docker compose -f "$REPO/deploy/compose.yaml" up -d >>"$LOG" 2>&1 || log "starting the compose services failed; see the log above"
  exit 0
fi

log "upgrade FAILED health check; rolling back. Last service logs:"
journalctl -u zenbot -n 30 --no-pager >> "$LOG" 2>&1
for b in $BINS; do [ -f "$BIN/$b.prev" ] && mv -f "$BIN/$b.prev" "$BIN/$b"; done
echo "$PREV_VERSION" > "$HOME/.zenbot/version"
sudo systemctl restart zenbot
if wait_healthy "$HEALTH" 45; then log "rolled back to previous version"; else log "ROLLBACK ALSO UNHEALTHY: check journalctl -u zenbot"; fi
if [ -n "$BACKUP" ]; then
  log "the new build may have applied migrations ${PENDING}(not undone). To restore the database as it was before: $(db_restore_cmd "$BACKUP")"
fi
exit 1
