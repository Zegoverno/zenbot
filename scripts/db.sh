#!/usr/bin/env bash
# SQLite helpers shared by install, development, upgrade and test scripts.
DB_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$DB_REPO/scripts/lib.sh"
DB_BACKUPS="${ZEN_HOME:-$HOME/.zenbot}/backups"
DB_KEEP=3

# The live database file, from DATABASE_URL (the environment, then ~/.zenbot/env).
db_live_path() {
  local url=${DATABASE_URL:-}
  [ -n "$url" ] || url=$(zen_env DATABASE_URL)
  url=${url:-sqlite://${ZEN_HOME:-$HOME/.zenbot}/zen.sqlite3}
  url=${url#sqlite://}; url=${url#sqlite:}
  echo "${url%%\?*}"
}
db_url_for() { echo "sqlite://${ZEN_DB_DIR:-${ZEN_HOME:-$HOME/.zenbot}}/$1.sqlite3"; }

db_query() {
  local url; url=$(db_url_for "$1")
  python3 "$DB_REPO/scripts/sqlite-query.py" "${url#sqlite://}" "$2"
}
db_create() {
  local url; url=$(db_url_for "$1")
  mkdir -p "$(dirname "${url#sqlite://}")"
  (umask 077; touch "${url#sqlite://}")
}
# Throwaway test databases only.
db_drop() {
  case "$1" in zen_e2e_*|zen_eval_*|zen_matrix_e2e_*) ;; *) echo 'refusing to drop a non-test database' >&2; return 1 ;; esac
  local path; path=$(db_url_for "$1"); path=${path#sqlite://}
  rm -f -- "$path" "$path-wal" "$path-shm"
}

# db_backup [label]: an online backup of the live database; prints its path. Keeps the newest DB_KEEP.
db_backup() {
  local path out label=${1:-manual}
  path=$(db_live_path)
  [ -f "$path" ] || return 0
  mkdir -p "$DB_BACKUPS" && chmod 700 "$DB_BACKUPS"
  out="$DB_BACKUPS/$(date -u +%Y%m%dT%H%M%S%N)-${label//[^A-Za-z0-9_-]/_}.sqlite3"
  python3 "$DB_REPO/scripts/storage.py" backup "$path" "$out" || return
  find "$DB_BACKUPS" -maxdepth 1 -name '*.sqlite3' -type f -printf '%T@ %p\n' | sort -rn | tail -n +$((DB_KEEP + 1)) | cut -d' ' -f2- | while IFS= read -r old; do rm -f -- "$old"; done
  echo "$out"
}
# db_restore BACKUP: put a backup back as the live database (the kernel must be stopped).
db_restore() {
  local path; path=$(db_live_path)
  rm -f -- "$path-wal" "$path-shm"
  install -m 600 "$1" "$path"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  case ${1:-} in
    backup) db_backup "${2:-manual}" ;;
    *) echo "usage: $0 backup [label]" >&2; exit 2 ;;
  esac
fi
