#!/usr/bin/env bash
# End-to-end test of the Matrix channel (docs/matrix.md), all local and free: a throwaway
# homeserver (continuwuity in Docker, no federation), a dev kernel on the scripted faux model, the
# bridge, and a scripted owner client (crates/zen-matrix/examples/owner.rs) talking to it through
# end-to-end encryption. Checks: the bot signs in with cross-signing and backup, invites the owner
# to two encrypted rooms, answers !help, relays a prompt and its answer, turns `ask` into numbered
# options and numbers back into answers, declines a stranger's invite, and posts a job's report.
#
#   scripts/matrix-e2e.sh        (builds crates/zen-matrix and the kernel first)
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/db.sh"
HS_PORT=${ZEN_MATRIX_E2E_HS_PORT:-16167}
K_PORT=${ZEN_MATRIX_E2E_PORT:-18197}
HS="http://127.0.0.1:$HS_PORT"
K="http://127.0.0.1:$K_PORT"
TMP=$(mktemp -d)
DB=zen_matrix_e2e_$$
NAME=zen-matrix-e2e-$$
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
  sudo docker rm -f "$NAME" >/dev/null 2>&1 || true
  db_drop "$DB" >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT
fail() { echo "FAILED: $*"; for f in kernel bridge; do echo "--- $f log"; tail -20 "$TMP/$f.log" 2>/dev/null; done; exit 1; }

echo "== build"
(cd "$REPO" && cargo build --release -q)
(cd "$REPO/crates/zen-matrix" && cargo build --release -q --bins --examples)
ZM="$REPO/crates/zen-matrix/target/release"

echo "== homeserver"
sudo docker run -d --name "$NAME" -p "127.0.0.1:$HS_PORT:6167" \
  -e CONTINUWUITY_SERVER_NAME=localhost -e CONTINUWUITY_DATABASE_PATH=/var/lib/continuwuity \
  -e CONTINUWUITY_ADDRESS=0.0.0.0 -e CONTINUWUITY_PORT=6167 -e CONTINUWUITY_ALLOW_REGISTRATION=true \
  -e CONTINUWUITY_REGISTRATION_TOKEN=e2e-token -e CONTINUWUITY_ALLOW_FEDERATION=false \
  ghcr.io/continuwuity/continuwuity:latest >/dev/null
for _ in $(seq 30); do curl -fs "$HS/_matrix/client/versions" >/dev/null && break; sleep 1; done
# The first account needs the one-time token the server prints; the configured token works after.
FIRST=
for _ in $(seq 30); do
  FIRST=$(sudo docker logs "$NAME" 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -o 'registration token [A-Za-z0-9]*' | awk '{print $3}' | head -1 || true)
  [ -n "$FIRST" ] && break
  sleep 1
done
[ -n "$FIRST" ] || fail "the homeserver printed no first-user registration token"
register() { # user password token
  local s
  s=$(curl -s -XPOST "$HS/_matrix/client/v3/register" -d "{\"username\":\"$1\",\"password\":\"$2\"}" | jq -r .session)
  curl -s -XPOST "$HS/_matrix/client/v3/register" -d "{\"username\":\"$1\",\"password\":\"$2\",\"auth\":{\"type\":\"m.login.registration_token\",\"token\":\"$3\",\"session\":\"$s\"}}" | jq -e .access_token >/dev/null || fail "registering $1"
}
register stranger pw-stranger-1 "$FIRST"
register zenbot pw-zenbot-1 e2e-token
register owner pw-owner-1 e2e-token

echo "== kernel (faux model)"
db_psql -d postgres -c "CREATE DATABASE $DB" >/dev/null
SCRIPT="$TMP/faux.json"
cat > "$SCRIPT" <<'EOF'
[
  { "when": "hello", "tool": "bash", "args": { "command": "echo zen-ok" } },
  { "when": "hello", "text": "Smoke test passed: I ran a command through the kernel." },
  { "when": "ask me", "tool": "ask", "args": { "questions": [ { "question": "Which color?", "options": ["red", "blue"] }, { "question": "Size?", "options": ["small", "large"] } ] } },
  { "when": "Which color? → blue", "text": "you chose: Which color? → blue" }
]
EOF
TOKEN=e2e-$$-$RANDOM
mkdir -p "$TMP/zenhome" "$TMP/ws"
(
  cd "$REPO"
  # The service's settings, as upgrade.sh's smoke test, minus everything paid: only the scripted engine.
  set -a; [ -f "$HOME/.zenbot/env" ] && . "$HOME/.zenbot/env"; set +a
  unset OPENROUTER_API_KEY ZEN_S1_MODEL BRAVE_API_KEY TAVILY_API_KEY
  ZEN_TOKEN="$TOKEN" ZEN_PORT=$K_PORT ZEN_WORKERS=engine ZEN_FAUX=1 ZEN_FAUX_SCRIPT="$SCRIPT" \
    ZEN_DEFAULT_MODEL=faux/smoke ZEN_SUGGEST=off ZEN_JOBS=0 ZEN_HOME="$TMP/zenhome" ZEN_WORKSPACE="$TMP/ws" \
    DATABASE_URL="$(db_url_for "$DB")" exec ./target/release/zend
) >"$TMP/kernel.log" 2>&1 &
PIDS+=($!)
wait_healthy "$K/health" 60 "${PIDS[-1]}" || fail "the kernel didn't come up"

echo "== bridge"
mkdir -p "$TMP/bridge"
cat > "$TMP/bridge/matrix.env" <<EOF
MATRIX_USER=@zenbot:localhost
MATRIX_PASSWORD=pw-zenbot-1
MATRIX_OWNER=@owner:localhost
MATRIX_HOMESERVER=$HS
EOF
export ZEN_HOME="$TMP/bridge" ZEN_URL="$K" ZEN_TOKEN="$TOKEN"
"$ZM/zen-matrix" login >"$TMP/bridge.log" 2>&1 || fail "zen-matrix login"
grep -q "cross-signing and key backup are set up" "$TMP/bridge.log" || fail "no cross-signing/backup"
[ "$(stat -c %a "$TMP/bridge/matrix")" = 700 ] && [ "$(stat -c %a "$TMP/bridge/matrix/session.json")" = 600 ] || fail "state files aren't private"
"$ZM/zen-matrix" run >>"$TMP/bridge.log" 2>&1 &
PIDS+=($!)
sleep 5

echo "== a stranger's invite is declined"
SAT=$(curl -s -XPOST "$HS/_matrix/client/v3/login" -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"stranger"},"password":"pw-stranger-1"}' | jq -r .access_token)
SRID=$(curl -s -XPOST -H "Authorization: Bearer $SAT" "$HS/_matrix/client/v3/createRoom" -d '{"name":"stranger","invite":["@zenbot:localhost"]}' | jq -r .room_id)

echo "== the owner talks to zen"
"$ZM/examples/owner" "$HS" owner pw-owner-1 "$TMP/owner" matrix-e2e >"$TMP/owner.log" 2>&1 &
OWNER=$!
PIDS+=($OWNER)
for _ in $(seq 120); do grep -q "questions answered" "$TMP/owner.log" && break; kill -0 $OWNER 2>/dev/null || break; sleep 1; done
# A job run now; the bridge reports it within a minute.
curl -fs -XPOST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' "$K/api/jobs" \
  -d '{"name":"matrix-e2e","prompt":"hello from a job","schedule":"every 24h","model":"faux/smoke"}' >/dev/null
curl -fs -XPOST -H "Authorization: Bearer $TOKEN" "$K/api/jobs/matrix-e2e/run" >/dev/null
wait $OWNER || { cat "$TMP/owner.log"; fail "the owner's conversation"; }
cat "$TMP/owner.log" | grep '^ok'

MEMBERSHIP=$(curl -s -H "Authorization: Bearer $SAT" "$HS/_matrix/client/v3/rooms/$SRID/members" | jq -r '.chunk[] | select(.state_key == "@zenbot:localhost") | .content.membership')
[ "$MEMBERSHIP" = leave ] || fail "the bot's membership in the stranger's room is '$MEMBERSHIP', not leave"
echo "ok: a stranger's invite was declined"
echo "PASSED"
