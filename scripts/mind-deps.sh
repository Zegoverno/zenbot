#!/usr/bin/env bash
# Install the Pi worker's dependencies (packages/mind) from the lockfile, keeping a newer Pi.
#
# The repo pins Pi's minimum version (package.json and package-lock.json). scripts/update-engines.sh
# installs newer Pi releases into node_modules only (npm install --no-save), so the daily update
# makes no commits. `npm ci` would put the pinned version back, so any Pi package that was installed
# at a newer version before it is reinstalled at that version after it.
# Used by install.sh, scripts/upgrade.sh and scripts/update-engines.sh.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../packages/mind"
PI_PKGS=(@earendil-works/pi-ai @earendil-works/pi-agent-core)
installed() { node -p "require('./node_modules/$1/package.json').version" 2>/dev/null || true; }
pinned() { jq -r --arg p "$1" '.packages["node_modules/" + $p].version // empty' package-lock.json; }
keep=()
for p in "${PI_PKGS[@]}"; do
  v=$(installed "$p"); pin=$(pinned "$p")
  if [ -n "$v" ] && [ -n "$pin" ] && [ "$v" != "$pin" ] && [ "$(printf '%s\n%s\n' "$pin" "$v" | sort -V | tail -1)" = "$v" ]; then
    keep+=("$p@$v")
  fi
done
npm ci --no-audit --no-fund --silent
if [ ${#keep[@]} -gt 0 ]; then npm install --no-save --no-audit --no-fund --silent "${keep[@]}"; fi
