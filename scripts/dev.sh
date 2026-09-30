#!/usr/bin/env bash
# Run zenbot locally: Postgres in Docker, zend + zen-mind on the host.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"

mkdir -p "$HOME/.zenbot"
if [ ! -f "$HOME/.zenbot/token" ]; then
  head -c 24 /dev/urandom | base64 | tr -d '/+=' > "$HOME/.zenbot/token"
  chmod 600 "$HOME/.zenbot/token"
fi
export ZEN_TOKEN="${ZEN_TOKEN:-$(cat "$HOME/.zenbot/token")}"
export ZEN_MIND_DIR="$PWD/packages/mind"
export ZEN_PORT="${ZEN_PORT:-8100}"

docker compose -f deploy/compose.yaml up -d --wait postgres
cargo build --release -q
exec ./target/release/zend
