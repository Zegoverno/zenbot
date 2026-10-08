#!/usr/bin/env bash
# The nightly sleep: asks the running kernel to tidy short-term memory (DESIGN.md, "Memory and
# knowledge"). Run by zen-sleep.timer (deploy/); safe to run by hand (`zen memory sleep` does the
# same as the owner). What it did is in the `sleep_runs` table, `zen memory` and `zen status`.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/lib.sh"
PORT=$(zen_env ZEN_PORT); PORT=${PORT:-8100}
TOKEN=$(cat "$HOME/.zenbot/token")
# The kernel may be restarting (an upgrade): wait for it first.
wait_healthy "http://127.0.0.1:$PORT/health" 300 || { echo "sleep: zenbot is not healthy; skipped" >&2; exit 1; }
# The token goes to curl on stdin, not argv (argv is visible to every user in `ps`).
printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" | curl -fsS --max-time 3600 -X POST -K - \
  "http://127.0.0.1:$PORT/api/memory/sleep?trigger=nightly" |
  jq -r '"sleep: \(.entries) entries, \(.kept) kept, \(.dropped) archived, \(.promoted) promoted (scorer: \(.scorer // "none, by recency"))"'
