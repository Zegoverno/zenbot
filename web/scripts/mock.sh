#!/usr/bin/env bash
# Serves the redesign mocks (web/mock/, fake data, no kernel) with hot reload for zen's Preview tab:
# open /preview/5190/ there. Usage: scripts/mock.sh [port]
set -euo pipefail
cd "$(dirname "$0")/.."
PORT="${1:-5190}"
exec npx vite mock --config vite.config.ts --host 127.0.0.1 --port "$PORT" --strictPort --base "/preview/$PORT/"
