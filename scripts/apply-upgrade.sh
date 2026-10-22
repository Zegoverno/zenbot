#!/usr/bin/env bash
# Runs detached (via systemd-run) from upgrade.sh with the stage it made (~/.zenbot/upgrades/<id>:
# the tested binaries, `commit`, and `note` if the tree had uncommitted changes): waits for idle,
# swaps in the staged binaries, brings ~/.zenbot/env up to date (a private database password,
# db_secure in scripts/db.sh; retired worker names out of ZEN_WORKERS; env.prev keeps the file as
# it was), restarts, health-checks, and rolls back on failure. Installs are
# serialized by a lock, and a request superseded by a newer one steps aside instead of installing
# over it, so the log and ~/.zenbot/version always name the commit actually running. If the new build brings migrations the
# live database hasn't applied, the database is backed up first (scripts/db.sh); a rollback
# doesn't restore it (migrations are expand-only, see AGENTS.md) but logs how to. After a healthy
# install it trims the checkout's cargo target directories over 2 GiB and prunes dangling Docker
# images and old build cache (trim_target, docker_prune in scripts/lib.sh).
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
      log "zenbot isn't answering $HEALTH; going ahead without waiting"
      return 0
    fi
    if ((SECONDS >= DEADLINE)); then
      log "zenbot still busy after 30 minutes; restarting anyway, which ends the running turns"
      return 0
    fi
    sleep 2
  done
}
wait_idle

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

# A session may have started during the backup: wait again, right before anything changes, so the
# restart doesn't end it (and a running kernel never starts workers from the new binaries).
wait_idle

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
env_backup
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
sudo systemctl restart zenbot

if wait_healthy "$HEALTH" 45; then
  log "upgrade OK: now running $COMMIT$NOTE"
  # The kernel's scheduler runs the sleep and engine updates now (D-046): the old timers go.
  remove_old_timers 2>>"$LOG" || log "removing the old zen-sleep/zen-engines timers failed; remove them by hand"
  # Services a new version brings in deploy/compose.yaml (e.g. SearXNG for web search).
  sudo docker compose -f "$REPO/deploy/compose.yaml" up -d >>"$LOG" 2>&1 || log "starting the compose services failed; see the log above"
  # Disk hygiene (the VM's disk once filled up): build directories over 2 GiB and old Docker
  # leftovers. The installed binaries are in ~/.zenbot/bin, so nothing here can break the install.
  SRC=$(zen_env ZEN_REPO); SRC=${SRC:-$HOME/zenbot}
  for t in "$SRC/target" "$SRC/crates/zen-matrix/target"; do
    TRIMMED=$(trim_target "$t" 2>>"$LOG") || true
    [ -z "$TRIMMED" ] || log "$TRIMMED"
  done
  docker_prune >>"$LOG" 2>&1 || true
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

log "upgrade FAILED health check; rolling back. Last service logs:"
journalctl -u zenbot -n 30 --no-pager >> "$LOG" 2>&1
restore_bins
echo "$PREV_VERSION" > "$HOME/.zenbot/version"
sudo systemctl restart zenbot
if wait_healthy "$HEALTH" 45; then log "rolled back to previous version"; else log "ROLLBACK ALSO UNHEALTHY: check journalctl -u zenbot"; fi
if [ -n "$BACKUP" ]; then
  log "the new build may have applied migrations ${PENDING}(not undone). To restore the database as it was before: $(db_restore_cmd "$BACKUP")"
fi
exit 1
