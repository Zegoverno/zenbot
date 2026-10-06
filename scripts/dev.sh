#!/usr/bin/env bash
# Run a development kernel from this checkout in the foreground: Postgres in Docker, zend (and,
# with the `pi` worker, zen-mind) on the host. It uses the service's settings (~/.zenbot/env) but
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

mkdir -p "$HOME/.zenbot"
if [ ! -f "$HOME/.zenbot/token" ]; then
  head -c 24 /dev/urandom | base64 | tr -d '/+=' > "$HOME/.zenbot/token"
  chmod 600 "$HOME/.zenbot/token"
fi
# The service's settings (workers, models, budgets), as upgrade.sh's smoke test does.
set -a; [ -f "$HOME/.zenbot/env" ] && . "$HOME/.zenbot/env"; set +a
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
ZEN_TOKEN="$(cat "$HOME/.zenbot/token")"
DATABASE_URL="$(db_url_for "$DB")"
ZEN_HARNESS="$(git rev-parse --short HEAD)" # turns record this checkout, not the installed version
export ZEN_TOKEN DATABASE_URL ZEN_HARNESS ZEN_PORT=$PORT ZEN_MIND_DIR="$REPO/packages/mind"

docker compose -f deploy/compose.yaml up -d --wait postgres
[ "$(db_psql -d postgres -c "SELECT 1 FROM pg_database WHERE datname = '$DB'")" = 1 ] ||
  db_psql -d postgres -c "CREATE DATABASE $DB"
# Pi's packages as packages/mind pins them (npm ci when the lockfile is newer than the install).
if [[ ",${ZEN_WORKERS:-}," == *",pi,"* ]] && [ ! packages/mind/node_modules/.package-lock.json -nt packages/mind/package-lock.json ]; then
  (cd packages/mind && npm ci --no-audit --no-fund --silent)
fi
cargo build --release -q
echo "dev kernel on http://127.0.0.1:$PORT (database $DB); use it with: ZEN_URL=http://127.0.0.1:$PORT ./target/release/zen"
exec ./target/release/zend
