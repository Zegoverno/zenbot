#!/usr/bin/env bash
# Detached, serialized install from immutable staged binaries/schema/tooling. Waits for idle,
# tests expand-only SQLite migrations on backed-up state, installs and health checks. A first
# PostgreSQL cutover requires positive idleness, stops the old writer, takes a final consistent
# archive and verifies the SQLite import before switching settings/service. Before SQLite startup,
# preparation failures may restore the old installation. After SQLite startup is attempted,
# PostgreSQL rollback is forbidden: retained data and new binaries stay for forward recovery.
# Later SQLite-only upgrades may restore the previous SQLite-capable binaries without restoring
# an older database. trim_target limits build disk usage after successful installs.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/db.sh"
BIN="$HOME/.zenbot/bin"
LOG="$HOME/.zenbot/upgrade.log"
SERVICE_FILE=${ZEN_UPGRADE_SERVICE_FILE:-/etc/systemd/system/zenbot.service}
PORT=$(zen_env ZEN_PORT); PORT=${PORT:-8100}
HEALTH="http://127.0.0.1:$PORT/health"
log() { echo "$(date -u +%FT%TZ) $*" >> "$LOG"; }

STAGE=${1:?usage: apply-upgrade.sh <stage dir from upgrade.sh>}
STAGES=$(dirname "$STAGE")
COMMIT=$(cat "$STAGE/commit" 2>/dev/null)
NOTE=$([ -f "$STAGE/note" ] && echo ", $(cat "$STAGE/note")")
if [ -z "$COMMIT" ]; then log "upgrade ABORTED: no staged build in $STAGE; nothing changed"; exit 1; fi
ZEN_STORAGE_TOOL="$STAGE/storage-tool/scripts/storage.py"
[ -f "$ZEN_STORAGE_TOOL" ] || { log "upgrade ABORTED: staged storage tooling missing"; exit 1; }
ZEN_PG_COMPOSE="$STAGE/compose.yaml"
CUTOVER= PG_STOPPED= NEW_STARTED= ENV_CHANGED= SERVICE_CHANGED=
db_is_postgres && CUTOVER=1
cleanup_stage() {
  # Only failures BEFORE the SQLite kernel starts can safely return to the retained PG writer.
  if [ -n "$PG_STOPPED" ] && [ -z "$NEW_STARTED" ]; then
    declare -F restore_bins >/dev/null && restore_bins
    [ -z "$ENV_CHANGED" ] || cp -p "$HOME/.zenbot/env.prev" "$HOME/.zenbot/env"
    if [ -n "$SERVICE_CHANGED" ] && [ -f "$STAGE/service.prev" ]; then
      sudo install -m 644 "$STAGE/service.prev" "$SERVICE_FILE"
      sudo systemctl daemon-reload
    fi
    sudo systemctl start zenbot
    log "cutover preparation failed before SQLite startup; previous PostgreSQL installation restarted"
  fi
  rm -rf "$STAGE"
}
trap cleanup_stage EXIT

log "upgrade requested ($COMMIT$NOTE)"
# Wait until no session is working. A kernel that doesn't answer at all (down, or crash-looping)
# has no turn to end, so that goes ahead at once; after 30 minutes it goes ahead anyway (a stuck
# session must not block upgrades forever), but says so.
DEADLINE=$((SECONDS + 1800))
wait_idle() {
  local h fails=0
  while :; do
    if h=$(curl -fs --max-time 10 "$HEALTH"); then
      fails=0
      grep -q '"busy":0' <<<"$h" && return 0
    elif ((++fails >= 5)); then
      if [ -n "$CUTOVER" ]; then
        log "cutover ABORTED: kernel health unavailable; cannot prove idleness"
        return 1
      fi
      log "zenbot isn't answering $HEALTH; going ahead without waiting"
      return 0
    fi
    if ((SECONDS >= DEADLINE)); then
      if [ -n "$CUTOVER" ]; then
        log "cutover ABORTED: kernel remained busy; refusing to interrupt turns for data migration"
        return 1
      fi
      log "zenbot still busy after 30 minutes; restarting anyway, which ends the running turns"
      return 0
    fi
    sleep 2
  done
}
wait_idle || exit 1

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
if [ -n "$CUTOVER" ]; then
  PENDING=1 # PostgreSQL history is archived separately; never replay it as SQLite migrations.
else
if ! PENDING=$(db_pending 2>>"$LOG" | tr '\n' ' '); then
  log "upgrade ABORTED: can't ask the database which migrations are pending (see above); nothing changed"
  exit 1
fi
if [ -n "$PENDING" ]; then
  if ! BACKUP=$(db_backup "${PREV_VERSION:-unknown}" 2>>"$LOG"); then
    log "upgrade ABORTED: migrations ${PENDING}pending but the database backup failed; nothing changed"
    exit 1
  fi
  log "migrations ${PENDING}pending; database backed up to $BACKUP"
fi

fi

# A session may have started during the backup: wait again, right before anything changes, so the
# restart doesn't end it (and a running kernel never starts workers from the new binaries).
wait_idle || exit 1

if [ -n "$CUTOVER" ]; then
  # Stop the old writer only from the detached upgrade, after a second positive idle check.
  # A final quiescent snapshot replaces the live smoke snapshot; PostgreSQL stays untouched.
  if ! sudo systemctl stop zenbot; then log "cutover ABORTED: cannot stop old writer"; exit 1; fi
  PG_STOPPED=1
  mkdir -p "$DB_BACKUPS" && chmod 700 "$DB_BACKUPS" || { log "cutover ABORTED: cannot create private archive directory"; exit 1; }
  BACKUP="$DB_BACKUPS/$(date -u +%Y%m%dT%H%M%S%N)-postgres-cutover.jsonl"
  SQLITE_PATH="${ZEN_HOME:-$HOME/.zenbot}/zen.sqlite3"
  [ ! -e "$SQLITE_PATH" ] || { log "cutover ABORTED: destination already exists; inspect it before retrying"; exit 1; }
  if ! db_pg_export "$BACKUP" 2>>"$LOG" || ! python3 "$ZEN_STORAGE_TOOL" restore "$BACKUP" "$SQLITE_PATH" 2>>"$LOG"; then
    log "cutover ABORTED: final archive/import validation failed; PostgreSQL and archive retained"
    exit 1
  fi
  log "quiescent PostgreSQL archive verified and imported; retained at $BACKUP"
fi

BINS="zend zen zen-engine"
# Put back the binaries replaced so far (all of them on a rollback).
restore_bins() { local b; for b in $BINS; do [ -f "$BIN/$b.prev" ] && mv -f "$BIN/$b.prev" "$BIN/$b"; done; }
rm -f "$BIN"/*.prev
for b in $BINS; do
  if ! { { [ ! -f "$BIN/$b" ] || cp -f "$BIN/$b" "$BIN/$b.prev"; } &&
    install -m 755 "$STAGE/$b" "$BIN/$b.new" && mv -f "$BIN/$b.new" "$BIN/$b"; } 2>>"$LOG"; then
    rm -f "$BIN/$b.new"
    restore_bins
    log "upgrade ABORTED: installing $b failed (see above); the previous binaries are back and zenbot wasn't restarted"
    exit 1
  fi
done

# Settings the new version needs, written right before the restart (old binaries read the same
# file, so a rollback keeps working): env.prev keeps env as it was.
env_backup || { log "upgrade ABORTED: retaining environment failed"; exit 1; }
if [ -n "$CUTOVER" ]; then
  ENV_CHANGED=1
  env_set DATABASE_URL "sqlite://$SQLITE_PATH" || { log "cutover ABORTED: setting SQLite URL failed"; exit 1; }
  cp "$SERVICE_FILE" "$STAGE/service.prev" || { log "cutover ABORTED: cannot retain service configuration"; exit 1; }
  sed -e "s#__USER__#$(id -un)#g" -e "s#__REPO__#$REPO#g" -e "s#__HOME__#$HOME#g" "$STAGE/zenbot.service" > "$STAGE/service.new"
  SERVICE_CHANGED=1
  sudo install -m 644 "$STAGE/service.new" "$SERVICE_FILE" && sudo systemctl daemon-reload || { log "cutover ABORTED: installing Docker-free service failed"; exit 1; }
fi
DB_SECURED=$(db_secure 2>>"$LOG") || log "making the database password private failed (see above); it stays as it was"
[ -z "$DB_SECURED" ] || log "$DB_SECURED"
# Pi was retired (D-043): an older kernel refused to start with it in ZEN_WORKERS.
WORKERS=$(zen_env ZEN_WORKERS)
if [[ ",$WORKERS," == *,pi,* ]]; then
  KEPT=$(tr ',' '\n' <<<"$WORKERS" | grep -vx pi | paste -sd, -)
  env_set ZEN_WORKERS "${KEPT:-engine}"
  log "removed the retired worker pi from ZEN_WORKERS in ~/.zenbot/env (now ${KEPT:-engine}; the old file is env.prev)"
fi

# The kernel reads the version at start and records it with every turn, so write it first.
echo "$COMMIT" > "$HOME/.zenbot/version"
# From this point the new kernel may write SQLite: returning to PostgreSQL would lose history.
NEW_STARTED=1
sudo systemctl restart zenbot

if wait_healthy "$HEALTH" 45; then
  touch "$BIN/.sqlite-storage"
  log "upgrade OK: now running $COMMIT$NOTE"
  # The kernel's scheduler runs the sleep and engine updates now (D-046): the old timers go.
  remove_old_timers 2>>"$LOG" || log "removing the old zen-sleep/zen-engines timers failed; remove them by hand"
  # Disk hygiene: trim build directories over 2 GiB. The installed binaries are in ~/.zenbot/bin, so nothing here can break the install.
  SRC=$(zen_env ZEN_REPO); SRC=${SRC:-$HOME/zenbot}
  for t in "$SRC/target" "$SRC/crates/zen-matrix/target"; do
    TRIMMED=$(trim_target "$t" 2>>"$LOG") || true
    [ -z "$TRIMMED" ] || log "$TRIMMED"
  done
  # zen-matrix, when upgrade.sh rebuilt it (D-077): swap it in, keep the old one, and put that
  # back if the bridge isn't running 10 seconds after its restart.
  if [ -f "$STAGE/zen-matrix" ]; then
    MV=$(cat "$STAGE/matrix-version" 2>/dev/null)
    { [ ! -f "$BIN/zen-matrix" ] || cp -f "$BIN/zen-matrix" "$BIN/zen-matrix.prev"; } \
      && install -m 755 "$STAGE/zen-matrix" "$BIN/zen-matrix.new" && mv -f "$BIN/zen-matrix.new" "$BIN/zen-matrix"
    sudo systemctl restart zen-matrix 2>>"$LOG"; sleep 10
    if systemctl is-active --quiet zen-matrix; then
      echo "$MV" > "$HOME/.zenbot/matrix.version"
      log "zen-matrix updated (sources at $MV)"
    else
      [ -f "$BIN/zen-matrix.prev" ] && mv -f "$BIN/zen-matrix.prev" "$BIN/zen-matrix"
      sudo systemctl restart zen-matrix 2>>"$LOG"
      log "zen-matrix update FAILED to start; the previous bridge is back (journalctl -u zen-matrix)"
    fi
  fi
  exit 0
fi

log "upgrade FAILED health check; checking rollback compatibility. Last service logs:"
journalctl -u zenbot -n 30 --no-pager >> "$LOG" 2>&1
# A backend cutover cannot roll back binaries to a PostgreSQL writer: that would fork history.
# Every future SQLite stage carries this marker; old (PostgreSQL) installed versions do not.
if [ -n "$CUTOVER" ] || [ ! -f "$BIN/.sqlite-storage" ]; then
  log "upgrade failed after SQLite startup; automatic rollback to PostgreSQL is forbidden. SQLite retained; recover using a SQLite-capable build."
  exit 1
fi
restore_bins
echo "$PREV_VERSION" > "$HOME/.zenbot/version"
sudo systemctl restart zenbot
if wait_healthy "$HEALTH" 45; then log "rolled back to previous version"; else log "ROLLBACK ALSO UNHEALTHY: check journalctl -u zenbot"; fi
if [ -n "$BACKUP" ]; then
  log "the new build may have applied migrations ${PENDING}(not undone). To restore the database as it was before: $(db_restore_cmd "$BACKUP")"
fi
exit 1
