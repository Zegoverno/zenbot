#!/usr/bin/env bash
# Run a development kernel from this checkout in the foreground: Postgres in Docker and zend
# on the host. It uses the service's settings (~/.zenbot/env) but
# its own database (zen_dev) and port (ZEN_DEV_PORT, default 18100), so it runs next to the
# installed service without touching its data.
#
#   scripts/dev.sh
#   ZEN_URL=http://127.0.0.1:18100 ./target/release/zen      # talk to it
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
. "$REPO/scripts/db.sh"
PORT=${ZEN_DEV_PORT:-18100}
DB=${ZEN_DEV_DB:-zen_dev}

new_token
# The service's settings (workers, models, budgets), as upgrade.sh's smoke test does.
set -a; [ -f "$HOME/.zenbot/env" ] && . "$HOME/.zenbot/env"; set +a
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
ZEN_TOKEN="$(cat "$HOME/.zenbot/token")"
DATABASE_URL="$(db_url_for "$DB")"
ZEN_HARNESS="$(git rev-parse --short HEAD)" # turns record this checkout, not the installed version
export ZEN_TOKEN DATABASE_URL ZEN_HARNESS ZEN_PORT=$PORT
# Keep the dev kernel outside the live home so the live file tree never exposes its prompt files.
# Move an existing dev home once without replacing anything at the new path.
if [ -z "${ZEN_DEV_HOME:-}" ] && [ -d "$HOME/.zenbot/dev" ]; then
  if [ -e "$HOME/.zenbot-dev" ]; then
    echo "Both ~/.zenbot/dev and ~/.zenbot-dev exist; move the old dev home by hand before starting" >&2
    exit 1
  fi
  mv "$HOME/.zenbot/dev" "$HOME/.zenbot-dev"
fi
export ZEN_HOME=${ZEN_DEV_HOME:-$HOME/.zenbot-dev}

docker compose -f deploy/compose.yaml up -d --wait postgres
[ "$(db_psql -d postgres -c "SELECT 1 FROM pg_database WHERE datname = '$DB'")" = 1 ] ||
  db_psql -d postgres -c "CREATE DATABASE $DB"
cargo build --release -q
echo "dev kernel on http://127.0.0.1:$PORT (database $DB); use it with: ZEN_URL=http://127.0.0.1:$PORT ./target/release/zen"
exec ./target/release/zend
