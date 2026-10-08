#!/usr/bin/env bash
# Install zenbot on a fresh Linux VM (Ubuntu/Debian with systemd and sudo). Safe to re-run.
#
#   git clone <repo-url> ~/zenbot && ~/zenbot/install.sh
#
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO"
. "$REPO/scripts/lib.sh"
say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
ENV_FILE="$HOME/.zenbot/env"
# Add KEY=VALUE to ~/.zenbot/env unless KEY is set there already, so a re-run keeps the owner's settings.
env_default() { grep -qE "^$1=" "$ENV_FILE" 2>/dev/null || echo "$1=$2" >> "$ENV_FILE"; }
# Set KEY=VALUE in ~/.zenbot/env, replacing any line for KEY (for settings passed to this run).
env_set() {
  local rest; rest=$(grep -vE "^$1=" "$ENV_FILE" 2>/dev/null || true)
  { [ -n "$rest" ] && echo "$rest"; echo "$1=$2"; } > "$ENV_FILE"
}

# Prebuilt binaries are downloaded when available (scripts/fetch-release.sh); Rust and a C
# toolchain are only installed when zenbot has to be compiled here. Set ZEN_BUILD_FROM_SOURCE=1
# to always compile.

say "System packages"
NEED=()
# bubblewrap: the read-only shell a verifier gets (crates/zend/src/tools.rs); poppler-utils:
# pdftotext, for PDFs web_fetch saves (crates/zend/src/web.rs).
for p in git curl ca-certificates jq bubblewrap poppler-utils; do dpkg -s "$p" >/dev/null 2>&1 || NEED+=("$p"); done
if ! command -v docker >/dev/null; then NEED+=(docker.io); fi
if ! docker compose version >/dev/null 2>&1 && ! sudo docker compose version >/dev/null 2>&1; then NEED+=(docker-compose-v2); fi
if [ ${#NEED[@]} -gt 0 ]; then
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${NEED[@]}"
fi
sudo systemctl enable --now docker >/dev/null 2>&1 || true
# Pull the database image in the background while the rest installs.
sudo docker compose -f deploy/compose.yaml pull -q >/tmp/zen-install-pull.log 2>&1 &
PULL_PID=$!
export PATH="$HOME/.cargo/bin:$PATH"

say "Node.js"
NODE_DIR="$HOME/.local/node"
if ! "$NODE_DIR/bin/node" --version 2>/dev/null | grep -qE '^v(2[2-9]|[3-9][0-9])\.'; then
  mkdir -p "$NODE_DIR"
  curl -fsSL https://nodejs.org/dist/v22.20.0/node-v22.20.0-linux-x64.tar.xz | tar -xJ -C "$NODE_DIR" --strip-components=1
fi
export PATH="$NODE_DIR/bin:$PATH"
grep -q '.local/node/bin' "$HOME/.bashrc" 2>/dev/null || echo 'export PATH="$HOME/.local/bin:$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"' >> "$HOME/.bashrc"

say "Model engines (Claude Code and Codex CLIs), in the background"
export PATH="$HOME/.local/bin:$PATH"
ENGINE_PIDS=()
if ! command -v claude >/dev/null; then (curl -fsSL https://claude.ai/install.sh | bash >/tmp/zen-install-claude.log 2>&1) & ENGINE_PIDS+=($!); fi
if ! command -v codex >/dev/null; then (npm install -g --no-audit --no-fund --silent @openai/codex >/tmp/zen-install-codex.log 2>&1) & ENGINE_PIDS+=($!); fi

# Keep configured model workers, but retire a stale `pi` entry from existing installations.
WORKERS="${ZEN_WORKERS:-$(zen_env ZEN_WORKERS)}"; WORKERS=${WORKERS:-engine}
WORKERS=$(printf '%s' "$WORKERS" | tr ',' '\n' | awk '$0 != "pi"' | paste -sd, -)
WORKERS=${WORKERS:-engine}
if "$REPO/scripts/fetch-release.sh"; then
  say "Downloaded prebuilt zenbot $(git rev-parse --short HEAD)"
else
  say "Building zenbot from source (no prebuilt binaries for this commit; takes several minutes)"
  ensure_rust
  cargo build --release -q
fi

for pid in "${ENGINE_PIDS[@]}"; do
  wait "$pid" || echo "warning: installing a model engine failed; see /tmp/zen-install-claude.log and /tmp/zen-install-codex.log"
done
wait "$PULL_PID" || true

say "Installing"
mkdir -p "$HOME/.zenbot/bin" "$HOME/.local/bin"
chmod 700 "$HOME/.zenbot"
install -m 755 target/release/zend target/release/zen target/release/zen-engine "$HOME/.zenbot/bin/"
ln -sf "$HOME/.zenbot/bin/zen" "$HOME/.local/bin/zen"
new_token
touch "$ENV_FILE"
chmod 600 "$ENV_FILE"
[ -z "${ZEN_PORT:-}" ] || env_set ZEN_PORT "$ZEN_PORT"
# The token file is the source of truth (the zen CLI reads it), so the service always gets it.
env_set ZEN_TOKEN "$(cat "$HOME/.zenbot/token")"
env_default ZEN_PORT 8100
env_default ZEN_REPO "$REPO"
env_set ZEN_WORKERS "$WORKERS"
env_default HOME "$HOME"
env_default PATH "$HOME/.local/bin:$NODE_DIR/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
PORT=$(zen_env ZEN_PORT)
git rev-parse --short HEAD > "$HOME/.zenbot/version" 2>/dev/null || true

say "Service"
sed -e "s#__USER__#$USER#g" -e "s#__REPO__#$REPO#g" -e "s#__HOME__#$HOME#g" deploy/zenbot.service \
  | sudo tee /etc/systemd/system/zenbot.service >/dev/null
install_timers "$REPO" # engine updates (daily) and the memory sleep (nightly)
sudo systemctl enable zenbot >/dev/null 2>&1
sudo systemctl restart zenbot
wait_healthy "http://127.0.0.1:$PORT/health" 60 || { echo "zenbot did not become healthy; see: journalctl -u zenbot -n 50"; exit 1; }

git -C "$REPO" config core.hooksPath scripts/git-hooks # commit trailers linking zen's commits to sessions

say "Done"
if [ -z "$(git -C "$REPO" config user.name)" ] || [ -z "$(git -C "$REPO" config user.email)" ]; then
  echo "Note: git has no identity, so commits zen makes will show a placeholder author. Set yours:"
  echo "  git config --global user.name \"Your Name\" && git config --global user.email you@example.com"
fi
echo "Next: sign in to Claude and ChatGPT with   zen login"
echo "Then run:   zen        (open a new shell first, or: export PATH=\$HOME/.local/bin:\$PATH)"
