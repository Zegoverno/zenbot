#!/usr/bin/env bash
# Offline PostgreSQL -> SQLite preparation. Does not stop/start services, change settings, remove
# PostgreSQL data, or overwrite a destination. Invoke from the upgrade cutover while zend is stopped.
# Credentials belong in PostgreSQL environment/service files, never in --psql-command arguments.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "${1:-}" != --offline ] || [ "$#" -lt 3 ]; then
  echo "usage: $0 --offline <new SQLite path> <private archive path> [psql command]" >&2
  exit 2
fi
DEST=$2 ARCHIVE=$3
PSQL_COMMAND=${4:-psql}
# A failed systemd connection is not evidence that the kernel is stopped.
STATE=$(systemctl is-active zenbot 2>/dev/null || true)
case "$STATE" in inactive|failed) ;; *) echo "Migration requires the upgrade cutover to stop zenbot first; service state is ${STATE:-unknown}. Nothing changed." >&2; exit 1 ;; esac
[ ! -e "$DEST" ] && [ ! -e "$ARCHIVE" ] || { echo 'Destination/archive already exists; refusing to overwrite.' >&2; exit 1; }
umask 077
mkdir -p "$(dirname "$DEST")" "$(dirname "$ARCHIVE")"
# Recheck after acquiring the same lock used by upgrade installation.
exec 8>"${ZEN_HOME:-$HOME/.zenbot}/storage-migration.lock"
flock -n 8 || { echo 'Another storage migration is active.' >&2; exit 1; }
STATE=$(systemctl is-active zenbot 2>/dev/null || true)
case "$STATE" in inactive|failed) ;; *) echo 'Kernel became active; aborting before export.' >&2; exit 1 ;; esac
python3 "$REPO/scripts/storage.py" export-postgres "$ARCHIVE" --psql-command "$PSQL_COMMAND"
python3 "$REPO/scripts/storage.py" restore "$ARCHIVE" "$DEST"
echo "Verified SQLite database prepared at $DEST; source PostgreSQL and archive retained."
echo 'Cut over DATABASE_URL and start only a SQLite-capable kernel. After its first write, do not restore PostgreSQL binaries/settings.'
