#!/usr/bin/env bash
# Database helpers for the upgrade scripts: Postgres runs in the deploy/compose.yaml container, so
# psql, pg_dump and pg_restore run inside it and nothing extra is needed on the host.
# Sourced by the other scripts (it also loads scripts/lib.sh); also usable by hand:
#
#   scripts/db.sh pending          migrations in this checkout the live database hasn't applied
#   scripts/db.sh backup [label]   dump the live database to ~/.zenbot/backups (keeps the last 10)
#   scripts/db.sh secure           give the database user a private password (db_secure, below)
#
# The live database is the one in DATABASE_URL (from ~/.zenbot/env if set there), else zend's
# default, `zen`. Docker runs without sudo when the user may use it, else with `sudo -n` (the
# standard install doesn't add the user to the docker group).
DB_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$DB_REPO/scripts/lib.sh"
DB_BACKUPS="$HOME/.zenbot/backups"
DB_KEEP=10

db_live_url() {
  local url=${DATABASE_URL:-}
  [ -n "$url" ] || url=$(zen_env DATABASE_URL)
  echo "${url:-postgres://zen:zen@127.0.0.1:5432/zen}"
}
db_live_name() { local n; n=$(db_live_url); n=${n##*/}; echo "${n%%\?*}"; }
# The live URL with another database name.
db_url_for() { local u; u=$(db_live_url); echo "${u%/*}/$1"; }

# docker, or `sudo -n docker` when the user can't reach the daemon (decided once).
DB_DOCKER=()
db_docker() {
  if [ ${#DB_DOCKER[@]} -eq 0 ]; then
    if docker info >/dev/null 2>&1; then DB_DOCKER=(docker); else DB_DOCKER=(sudo -n docker); fi
  fi
  "${DB_DOCKER[@]}" "$@"
}
db_exec() { db_docker compose -f "$DB_REPO/deploy/compose.yaml" exec -T postgres "$@"; }
db_psql() { db_exec psql -U zen -v ON_ERROR_STOP=1 -qAt "$@"; }

# Versions of the migration files in this checkout that the live database hasn't applied, one per
# line (all of them for a database zend never started on). Fails, saying why, when it can't ask.
db_pending() {
  local applied f v
  if ! applied=$(db_psql -d "$(db_live_name)" -c "SELECT to_regclass('_sqlx_migrations') IS NOT NULL" 2>&1); then
    echo "db_pending: can't query the database $(db_live_name): $applied" >&2
    return 1
  fi
  if [ "$applied" = t ]; then
    applied=$(db_psql -d "$(db_live_name)" -c "SELECT version FROM _sqlx_migrations WHERE success") || return 1
  else
    applied=""
  fi
  for f in "$DB_REPO"/crates/zend/migrations/*.sql; do
    v=$(basename "$f"); v=${v%%_*}; v=$((10#$v))
    grep -qx "$v" <<<"$applied" || echo "$v"
  done
}

# Dump the live database to $DB_BACKUPS/<UTC time>-<label>.dump; prints the path. Keeps the last $DB_KEEP.
db_backup() {
  local label=${1:-manual} out
  mkdir -p "$DB_BACKUPS"
  out="$DB_BACKUPS/$(date -u +%Y%m%dT%H%M%SZ)-$label.dump"
  if ! db_exec pg_dump -U zen -Fc "$(db_live_name)" >"$out" || [ ! -s "$out" ]; then
    rm -f "$out"; return 1
  fi
  ls -1t "$DB_BACKUPS"/*.dump 2>/dev/null | tail -n +$((DB_KEEP + 1)) | xargs -r rm -f
  echo "$out"
}

# Give the database user a private password, once per install: Postgres starts with the public
# bootstrap password in deploy/compose.yaml (`zen`), which any process on the VM could use to rewrite
# zenbot's state. This makes a random one in ~/.zenbot/db-password (private; secret to the kernel's
# tools), sets it with ALTER USER over the container's local socket (fed on stdin, so it never shows
# in a process list or a log), then points DATABASE_URL in ~/.zenbot/env at it (callers keep the
# old file with env_backup first). Done when DATABASE_URL already carries it; leaves a DATABASE_URL
# for another server or user alone. install.sh runs it, and apply-upgrade.sh right before the restart (old binaries read
# DATABASE_URL from env too, so a rollback keeps working). Prints what it did, never the password.
DB_PASSWORD_FILE="$HOME/.zenbot/db-password"
db_secure() {
  local cur pw want
  cur=$(zen_env DATABASE_URL)
  if [ -n "$cur" ] && ! [[ $cur =~ ^postgres(ql)?://zen(:[^@]*)?@(127\.0\.0\.1|localhost):5432/ ]]; then
    return 0 # not the bundled Postgres
  fi
  if [ ! -s "$DB_PASSWORD_FILE" ]; then
    mkdir -p "$(dirname "$DB_PASSWORD_FILE")"
    (umask 077 && head -c 48 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 32 > "$DB_PASSWORD_FILE")
  fi
  chmod 600 "$DB_PASSWORD_FILE"
  pw=$(cat "$DB_PASSWORD_FILE")
  [[ $pw =~ ^[A-Za-z0-9]{16,}$ ]] || { echo "db_secure: $DB_PASSWORD_FILE isn't a password this script made" >&2; return 1; }
  want="postgres://zen:$pw@127.0.0.1:5432/$(db_live_name)"
  [ "$cur" != "$want" ] || return 0
  printf "ALTER USER zen PASSWORD '%s';\n" "$pw" | db_exec psql -U zen -d postgres -v ON_ERROR_STOP=1 -q >/dev/null || {
    echo "db_secure: setting the database password failed" >&2; return 1; }
  env_set DATABASE_URL "$want"
  echo "database password replaced with a private one (~/.zenbot/db-password); DATABASE_URL updated in ~/.zenbot/env"
}

# How to put a backup back (stop zenbot first). Printed, never run by the scripts.
db_restore_cmd() {
  echo "sudo systemctl stop zenbot && sudo docker compose -f $DB_REPO/deploy/compose.yaml exec -T postgres pg_restore -U zen --clean --if-exists -d $(db_live_name) < $1 && sudo systemctl start zenbot"
}

# Copy the live database into a new database <name> (for the upgrade's smoke test).
db_copy_live() {
  db_psql -d postgres -c "CREATE DATABASE $1" >/dev/null &&
    db_exec sh -c "pg_dump -U zen -Fc '$(db_live_name)' | pg_restore -U zen --no-owner -d '$1'"
}
db_drop() { db_psql -d postgres -c "DROP DATABASE IF EXISTS $1 WITH (FORCE)" >/dev/null 2>&1; }

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  case ${1:-} in
    pending) db_pending ;;
    backup) db_backup "${2:-manual}" ;;
    secure) env_backup; db_secure ;;
    *) echo "usage: $0 pending | backup [label] | secure" >&2; exit 2 ;;
  esac
fi
