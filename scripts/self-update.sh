#!/usr/bin/env bash
# Update zenbot to the latest main and apply it: pull, then scripts/upgrade.sh (which uses
# CI's prebuilt binaries when ready, smoke-tests, and restarts once no session is working).
# Refuses to touch a checkout with local changes or on another branch. Used by `zen upgrade`
# and /upgrade; safe to run by hand.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "Not updating: $REPO has local changes. Commit or stash them first (git status shows them)."
  exit 2
fi
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [ "$BRANCH" != "main" ]; then
  echo "Not updating: $REPO is on branch '$BRANCH', not main."
  exit 2
fi
echo "== pull"
git pull --ff-only -q origin main || { echo "Not updating: main has diverged from origin/main; resolve it by hand."; exit 2; }
echo "now at $(git log -1 --format='%h %s')"
exec ./scripts/upgrade.sh
