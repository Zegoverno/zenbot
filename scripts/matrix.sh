#!/usr/bin/env bash
# Build and install zen-matrix, the Matrix channel (docs/matrix.md), and (re)start its service.
# It's a separate Cargo workspace (crates/zen-matrix), built here rather than by CI's release.
#
#   scripts/matrix.sh            build, test, install the binary and service, sign in if needed, restart
#   scripts/matrix.sh --build    build and test only
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/lib.sh"
ensure_rust
cd "$REPO/crates/zen-matrix"
echo "== build (the first build of matrix-sdk takes a while)"
cargo build --release --locked -q
cargo test --release --locked -q 2>&1 | tail -3
[ "${1:-}" = "--build" ] && exit 0

echo "== install"
mkdir -p "$HOME/.zenbot/bin"
install -m 755 target/release/zen-matrix "$HOME/.zenbot/bin/zen-matrix.new" && mv -f "$HOME/.zenbot/bin/zen-matrix.new" "$HOME/.zenbot/bin/zen-matrix"
[ -f "$HOME/.zenbot/matrix.env" ] || { echo "Next: write ~/.zenbot/matrix.env (MATRIX_USER, MATRIX_PASSWORD, MATRIX_OWNER; docs/matrix.md), then run this again."; exit 0; }
chmod 600 "$HOME/.zenbot/matrix.env"
[ -f "$HOME/.zenbot/matrix/session.json" ] || "$HOME/.zenbot/bin/zen-matrix" login

echo "== service"
sed -e "s#__USER__#$USER#g" -e "s#__HOME__#$HOME#g" "$REPO/deploy/zen-matrix.service" | sudo tee /etc/systemd/system/zen-matrix.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable zen-matrix >/dev/null 2>&1
sudo systemctl restart zen-matrix
sleep 5
systemctl is-active --quiet zen-matrix && echo "zen-matrix is running (logs: journalctl -u zen-matrix -f)" || { journalctl -u zen-matrix -n 30 --no-pager; exit 1; }
