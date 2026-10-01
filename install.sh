#!/usr/bin/env bash
# Install zenbot on a fresh Linux VM (Ubuntu/Debian with systemd and sudo). Safe to re-run.
#
#   git clone <repo-url> ~/zenbot && ~/zenbot/install.sh
#
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO"
say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

say "System packages"
NEED=()
for p in git curl ca-certificates build-essential pkg-config; do dpkg -s "$p" >/dev/null 2>&1 || NEED+=("$p"); done
if ! command -v docker >/dev/null; then NEED+=(docker.io); fi
if ! docker compose version >/dev/null 2>&1 && ! sudo docker compose version >/dev/null 2>&1; then NEED+=(docker-compose-v2); fi
if [ ${#NEED[@]} -gt 0 ]; then
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${NEED[@]}"
fi
sudo systemctl enable --now docker >/dev/null 2>&1 || true

say "Rust"
if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
fi
export PATH="$HOME/.cargo/bin:$PATH"

say "Node.js"
NODE_DIR="$HOME/.local/node"
if ! "$NODE_DIR/bin/node" --version 2>/dev/null | grep -qE '^v(2[2-9]|[3-9][0-9])\.'; then
  mkdir -p "$NODE_DIR"
  curl -fsSL https://nodejs.org/dist/v22.20.0/node-v22.20.0-linux-x64.tar.xz | tar -xJ -C "$NODE_DIR" --strip-components=1
fi
export PATH="$NODE_DIR/bin:$PATH"
grep -q '.local/node/bin' "$HOME/.bashrc" 2>/dev/null || echo 'export PATH="$HOME/.local/bin:$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"' >> "$HOME/.bashrc"

say "Building zenbot (first build takes a few minutes)"
(cd packages/mind && npm ci --no-audit --no-fund --silent)
cargo build --release -q

say "Installing"
mkdir -p "$HOME/.zenbot/bin" "$HOME/.local/bin"
chmod 700 "$HOME/.zenbot"
install -m 755 target/release/zend target/release/zen "$HOME/.zenbot/bin/"
ln -sf "$HOME/.zenbot/bin/zen" "$HOME/.local/bin/zen"
[ -f "$HOME/.zenbot/token" ] || head -c 24 /dev/urandom | base64 | tr -d '/+=' > "$HOME/.zenbot/token"
chmod 600 "$HOME/.zenbot/token"
cat > "$HOME/.zenbot/env" <<ENV
ZEN_TOKEN=$(cat "$HOME/.zenbot/token")
ZEN_PORT=${ZEN_PORT:-8100}
ZEN_REPO=$REPO
ZEN_MIND_DIR=$REPO/packages/mind
HOME=$HOME
PATH=$HOME/.local/bin:$NODE_DIR/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin
ENV
chmod 600 "$HOME/.zenbot/env"
git rev-parse --short HEAD > "$HOME/.zenbot/version" 2>/dev/null || true

say "Service"
sed -e "s#__USER__#$USER#g" -e "s#__REPO__#$REPO#g" -e "s#__HOME__#$HOME#g" deploy/zenbot.service \
  | sudo tee /etc/systemd/system/zenbot.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable zenbot >/dev/null 2>&1
sudo systemctl restart zenbot
for _ in $(seq 1 60); do
  curl -fs "http://127.0.0.1:${ZEN_PORT:-8100}/health" | grep -q '"ok":true' && break
  sleep 1
done
curl -fs "http://127.0.0.1:${ZEN_PORT:-8100}/health" | grep -q '"ok":true' || { echo "zenbot did not become healthy; see: journalctl -u zenbot -n 50"; exit 1; }

say "Done"
if [ ! -f "$HOME/.zenbot/auth.json" ]; then
  echo "Next: sign in to ChatGPT with   zen login"
fi
echo "Then run:   zen        (open a new shell first, or: export PATH=\$HOME/.local/bin:\$PATH)"
