#!/usr/bin/env bash
# Install zenbot as a systemd service that starts on boot and restarts on failure.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"

cargo build --release -q
mkdir -p "$HOME/.zenbot" "$HOME/.local/bin"
[ -f "$HOME/.zenbot/token" ] || { head -c 24 /dev/urandom | base64 | tr -d '/+=' > "$HOME/.zenbot/token"; }
chmod 600 "$HOME/.zenbot/token"
cat > "$HOME/.zenbot/env" <<ENV
ZEN_TOKEN=$(cat "$HOME/.zenbot/token")
ZEN_PORT=${ZEN_PORT:-8100}
ZEN_MIND_DIR=$REPO/packages/mind
HOME=$HOME
PATH=$HOME/.local/node/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin
ENV
chmod 600 "$HOME/.zenbot/env"
ln -sf "$REPO/target/release/zen" "$HOME/.local/bin/zen"

sed -e "s#__USER__#$USER#g" -e "s#__REPO__#$REPO#g" -e "s#__HOME__#$HOME#g" deploy/zenbot.service \
  | sudo tee /etc/systemd/system/zenbot.service > /dev/null
sudo systemctl enable --now docker.service > /dev/null 2>&1 || true
sudo systemctl daemon-reload
sudo systemctl enable zenbot.service > /dev/null
sudo systemctl restart zenbot.service
echo "zenbot service installed. Check with: zen status"
