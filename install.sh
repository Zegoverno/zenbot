#!/usr/bin/env bash
# Install zenbot on a fresh Linux VM (Ubuntu/Debian with systemd and sudo). Safe to re-run.
#
#   git clone <repo-url> ~/zenbot && ~/zenbot/install.sh
#
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO"
. "$REPO/scripts/db.sh" # and scripts/lib.sh
case $(db_live_url) in
  postgres:*|postgresql:*) echo "Existing PostgreSQL installation: use the quiescent storage migration before installing." >&2; exit 1 ;;
esac
say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
ENV_FILE="$HOME/.zenbot/env"
# Add KEY=VALUE to ~/.zenbot/env unless KEY is set there already, so a re-run keeps the owner's settings.
env_default() { grep -qE "^$1=" "$ENV_FILE" 2>/dev/null || echo "$1=$2" >> "$ENV_FILE"; }
# env_set (scripts/lib.sh) replaces KEY's line, for settings passed to this run.

# Prebuilt binaries are downloaded when available (scripts/fetch-release.sh); Rust and a C
# toolchain are only installed when zenbot has to be compiled here. Set ZEN_BUILD_FROM_SOURCE=1
# to always compile.

say "System packages"
NEED=()
# bubblewrap: the read-only shell a verifier gets (crates/zend/src/tools.rs); poppler-utils:
# pdftotext, for PDFs web_fetch saves (crates/zend/src/web.rs).
for p in git curl ca-certificates jq bubblewrap poppler-utils python3; do dpkg -s "$p" >/dev/null 2>&1 || NEED+=("$p"); done
if [ ${#NEED[@]} -gt 0 ]; then
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${NEED[@]}"
fi
export PATH="$HOME/.cargo/bin:$PATH"

say "Node.js"
NODE_DIR="$HOME/.local/node"
# Pinned, and checked against the SHA-256 nodejs.org publishes for it (SHASUMS256.txt for the
# version), so a tampered or truncated download is never unpacked. Bump both together.
NODE_VERSION=v22.20.0
# 64-bit Linux on x86_64 or ARM (a Raspberry Pi with 64-bit Pi OS, D-077).
case "$(uname -m)" in
  x86_64|amd64) NODE_ARCH=x64 NODE_SHA256=00bbd05e306ea68b6e13e17360d0e2f680b493ef95f2fea1c4296ff7437530bc ;;
  aarch64|arm64) NODE_ARCH=arm64 NODE_SHA256=06907b9c088ce62305bc1530e5c1ae1510245114645768f7750c349c5b6fe667 ;;
  *) echo "zenbot runs on 64-bit Linux (x86_64 or aarch64); this is $(uname -m)" >&2; exit 1 ;;
esac
NODE_TAR=node-$NODE_VERSION-linux-$NODE_ARCH.tar.xz
if ! "$NODE_DIR/bin/node" --version 2>/dev/null | grep -qE '^v(2[2-9]|[3-9][0-9])\.'; then
  mkdir -p "$NODE_DIR"
  NODE_DL=$(mktemp -d)
  curl -fsSL -o "$NODE_DL/$NODE_TAR" "https://nodejs.org/dist/$NODE_VERSION/$NODE_TAR"
  if ! echo "$NODE_SHA256  $NODE_DL/$NODE_TAR" | sha256sum -c --quiet -; then
    echo "The Node.js download doesn't match its published checksum; stopping." >&2
    exit 1
  fi
  tar -xJf "$NODE_DL/$NODE_TAR" -C "$NODE_DIR" --strip-components=1
  rm -rf "$NODE_DL"
fi
export PATH="$NODE_DIR/bin:$PATH"
grep -q '.local/node/bin' "$HOME/.bashrc" 2>/dev/null || echo 'export PATH="$HOME/.local/bin:$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"' >> "$HOME/.bashrc"

say "Model engines (Claude Code and Codex CLIs), in the background"
export PATH="$HOME/.local/bin:$PATH"
ENGINE_PIDS=()
if ! command -v claude >/dev/null; then (curl -fsSL https://claude.ai/install.sh | bash >/tmp/zen-install-claude.log 2>&1) & ENGINE_PIDS+=($!); fi
if ! command -v codex >/dev/null; then (npm install -g --no-audit --no-fund --silent @openai/codex >/tmp/zen-install-codex.log 2>&1) & ENGINE_PIDS+=($!); fi

WORKERS="${ZEN_WORKERS:-$(zen_env ZEN_WORKERS)}"; WORKERS=${WORKERS:-engine}
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

say "Installing"
mkdir -p "$HOME/.zenbot/bin" "$HOME/.local/bin"
chmod 700 "$HOME/.zenbot"
install -m 755 target/release/zend target/release/zen target/release/zen-engine "$HOME/.zenbot/bin/"
touch "$HOME/.zenbot/bin/.sqlite-storage"
ln -sf "$HOME/.zenbot/bin/zen" "$HOME/.local/bin/zen"
new_token
touch "$ENV_FILE"
chmod 600 "$ENV_FILE"
env_backup
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

say "Database"


say "Service"
sed -e "s#__USER__#$USER#g" -e "s#__REPO__#$REPO#g" -e "s#__HOME__#$HOME#g" deploy/zenbot.service \
  | sudo tee /etc/systemd/system/zenbot.service >/dev/null
remove_old_timers # the kernel schedules engine updates and the memory sleep itself (zen jobs)
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
