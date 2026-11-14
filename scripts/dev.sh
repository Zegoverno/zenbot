#!/usr/bin/env bash
# Run a development kernel from this checkout in the foreground: SQLite and zend
# on the host. It uses the service's settings (~/.zenbot/env: workers, models, budgets) but its own
# database (zen_dev), port (ZEN_DEV_PORT, default 18100) and home (~/.zenbot-dev, or ZEN_DEV_HOME,
# holding its workspace, wiki, skills, tools and MCP config), and none of the paid API keys, so it
# runs next to the installed service without touching its data or spending the owner's money.
# ZEN_DEV_REAL=1 keeps the keys and the service's folders.
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
if [ -z "${ZEN_DEV_REAL:-}" ]; then
  # Scoring, embedding and search with the owner's keys cost real money for nothing tested; the
  # folders below default to the dev home (set further down); the API listens on 127.0.0.1.
  unset OPENROUTER_API_KEY ZEN_S1_MODEL BRAVE_API_KEY TAVILY_API_KEY \
    ZEN_WORKSPACE ZEN_WIKI_DIR ZEN_SKILLS_DIR ZEN_TOOLS_DIR ZEN_MCP_CONFIG ZEN_BIND
fi
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
ZEN_TOKEN="$(cat "$HOME/.zenbot/token")"
DATABASE_URL="sqlite://${ZEN_DEV_HOME:-$HOME/.zenbot-dev}/$DB.sqlite3"
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
[ -n "${ZEN_DEV_REAL:-}" ] || { export ZEN_WORKSPACE=$ZEN_HOME/workspace; mkdir -p "$ZEN_WORKSPACE"; }
# A dev kernel doesn't run scheduled jobs (they would update the real CLIs); ZEN_JOBS=1 turns them on.
export ZEN_JOBS=${ZEN_JOBS:-0}

mkdir -p "$ZEN_HOME"
cargo build --release -q
echo "dev kernel on http://127.0.0.1:$PORT (database $DB); use it with: ZEN_URL=http://127.0.0.1:$PORT ./target/release/zen"
exec ./target/release/zend
