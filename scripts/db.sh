#!/usr/bin/env bash
# SQLite helpers shared by install, development and upgrade scripts. Online backups include WAL
# state using SQLite's backup API. PostgreSQL survives only as an explicit read-only migration source.
DB_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$DB_REPO/scripts/lib.sh"
DB_BACKUPS="${ZEN_HOME:-$HOME/.zenbot}/backups"
DB_KEEP=3
ZEN_STORAGE_TOOL=${ZEN_STORAGE_TOOL:-$DB_REPO/scripts/storage.py}

db_live_url() {
  local url=${DATABASE_URL:-}
  [ -n "$url" ] || url=$(zen_env DATABASE_URL)
  if [ -z "$url" ] && [ -x "$HOME/.zenbot/bin/zend" ] && [ ! -f "$HOME/.zenbot/bin/.sqlite-storage" ]; then
    url=postgres://zen:zen@127.0.0.1:5432/zen
  fi
  echo "${url:-sqlite://${ZEN_HOME:-$HOME/.zenbot}/zen.sqlite3}"
}
db_is_postgres() { [[ $(db_live_url) == postgres:* || $(db_live_url) == postgresql:* ]]; }
db_pg_export() {
  local url name
  url=$(db_live_url)
  if [[ $url =~ ^postgres(ql)?://[^@]*@(127\.0\.0\.1|localhost):5432/ ]]; then
    name=${url##*/}; name=${name%%\?*}
    [[ $name =~ ^[A-Za-z0-9_]+$ ]] || { echo 'Unsupported PostgreSQL database name.' >&2; return 1; }
    # Bundled PostgreSQL accepts its owner through the container-local socket. No URL/password
    # is passed to a process argument or log. Compose is only a retained migration source.
    local docker_command=docker
    docker info >/dev/null 2>&1 || docker_command='sudo -n docker'
    python3 "$ZEN_STORAGE_TOOL" export-postgres "$1" --psql-command "$docker_command compose -f ${ZEN_PG_COMPOSE:-$DB_REPO/deploy/compose.yaml} exec -T postgres psql -U zen -d $name"
  else
    ZEN_MIGRATION_PG_URL="$url" python3 "$ZEN_STORAGE_TOOL" export-postgres "$1" --pg-url-env ZEN_MIGRATION_PG_URL
  fi
}
db_live_path() {
  local url; url=$(db_live_url)
  case "$url" in
    sqlite://*) url=${url#sqlite://} ;;
    sqlite:*) url=${url#sqlite:} ;;
    *) echo 'PostgreSQL requires an explicit quiescent export/import before SQLite cutover; no data was changed.' >&2; return 1 ;;
  esac
  echo "${url%%\?*}"
}
db_live_name() { basename "$(db_live_path)"; }
db_url_for() { echo "sqlite://${ZEN_DB_DIR:-${ZEN_HOME:-$HOME/.zenbot}}/$1.sqlite3"; }
db_secure() { :; } # SQLite files/directories are private; no database account or daemon.

db_query() {
  local url; url=$(db_url_for "$1")
  python3 "$DB_REPO/scripts/sqlite-query.py" "${url#sqlite://}" "$2"
}
db_create() {
  local url; url=$(db_url_for "$1")
  mkdir -p "$(dirname "${url#sqlite://}")"
  (umask 077; touch "${url#sqlite://}")
}

db_pending() {
  local path; path=$(db_live_path) || return
  python3 - "$path" "$DB_REPO/crates/zend/sqlite-migrations" <<'PY'
import pathlib, sqlite3, sys
path, directory = sys.argv[1:]
applied = set()
if pathlib.Path(path).exists():
    with sqlite3.connect(f'file:{path}?mode=ro', uri=True) as db:
        if db.execute("SELECT 1 FROM sqlite_master WHERE name='_sqlx_migrations'").fetchone():
            applied = {r[0] for r in db.execute('SELECT version FROM _sqlx_migrations WHERE success')}
for migration in sorted(pathlib.Path(directory).glob('*.sql')):
    version = int(migration.name.split('_')[0])
    if version not in applied:
        print(version)
PY
}
db_backup() {
  local path out label=${1:-manual}
  path=$(db_live_path) || return
  mkdir -p "$DB_BACKUPS"
  chmod 700 "$DB_BACKUPS"
  out="$DB_BACKUPS/$(date -u +%Y%m%dT%H%M%S%N)-${label//[^A-Za-z0-9_-]/_}.sqlite3"
  python3 "$ZEN_STORAGE_TOOL" backup "$path" "$out" || return
  # Retention only applies to SQLite backups, never retained PostgreSQL recovery dumps.
  find "$DB_BACKUPS" -maxdepth 1 -name '*.sqlite3' -type f -printf '%T@ %p\n' | sort -rn | tail -n +$((DB_KEEP + 1)) | cut -d' ' -f2- | while IFS= read -r old; do rm -f -- "$old"; done
  echo "$out"
}
db_restore_cmd() {
  echo "Stop the kernel, preserve the current SQLite database and its -wal/-shm files, then restore $1 using scripts/storage.py backup into a new database path. Never switch back to retained PostgreSQL after SQLite writes."
}
db_copy_live() {
  local source destination
  destination=$(db_url_for "$1"); destination=${destination#sqlite://}
  mkdir -p "$(dirname "$destination")"
  if db_is_postgres; then
    db_pg_export "$destination.jsonl" || return
    python3 "$ZEN_STORAGE_TOOL" restore "$destination.jsonl" "$destination"
  else
    source=$(db_live_path) || return
    python3 "$ZEN_STORAGE_TOOL" backup "$source" "$destination"
  fi
}
db_drop() {
  # Only disposable smoke/e2e copies may be removed through this helper.
  case "$1" in zen_smoke_*|zen_e2e_*|zen_eval_*|zen_matrix_e2e_*) ;; *) echo 'refusing to drop a non-test database' >&2; return 1 ;; esac
  local path; path=$(db_url_for "$1"); path=${path#sqlite://}
  rm -f -- "$path" "$path-wal" "$path-shm" "$path.jsonl"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  case ${1:-} in
    pending) db_pending ;;
    backup) db_backup "${2:-manual}" ;;
    secure) db_secure ;;
    *) echo "usage: $0 pending | backup [label] | secure" >&2; exit 2 ;;
  esac
fi
