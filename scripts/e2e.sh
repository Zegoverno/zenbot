#!/usr/bin/env bash
# End-to-end tests of the kernel with the scripted faux model: each scenario runs real turns through
# a kernel built from this checkout, on a throwaway database and workspace, and checks the result in
# the database. No subscription is used and the live service isn't touched.
#
#   scripts/e2e.sh              build, then run every scenario
#   scripts/e2e.sh memory       only scenarios whose name (or a part's function name) contains "memory"
#
# Each scenario has its own database, HOME and ports (ZEN_E2E_PORT + 20 × its number, and the next
# few for its test servers), so ZEN_E2E_JOBS of them (default 4) run at once; ZEN_E2E_JOBS=1 runs
# them one by one. ZEN_E2E_NO_BUILD=1 skips the build; ZEN_E2E_BIN=<dir> tests the binaries there
# instead of target/release (CI's release job tests the ones it publishes); ZEN_E2E_KEEP=1 keeps
# the databases and files.
#
# Needs Python 3 with SQLite FTS5, git, curl, jq, bubblewrap and passt.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.cargo/bin:$PATH"
. "$REPO/scripts/db.sh"
FILTER=${1:-}
BASE_PORT=${ZEN_E2E_PORT:-18377}
JOBS=${ZEN_E2E_JOBS:-4}
TOKEN=e2e-token-$$
ROOT=$(mktemp -d)
BIN=$(cd "${ZEN_E2E_BIN:-$REPO/target/release}" && pwd)
KERNEL_PID=""
FAILED=0
# Set per scenario by in_scenario: its port, database and files.
PORT="" URL="" DB="" DB_URL="" TMP=""

q() { db_query "$DB" "$1"; }
stop_kernel() { if [ -n "$KERNEL_PID" ]; then kill "$KERNEL_PID" 2>/dev/null || true; wait "$KERNEL_PID" 2>/dev/null || true; fi; KERNEL_PID=""; }
cleanup() {
  local pids; pids=$(jobs -p); [ -z "$pids" ] || { kill $pids 2>/dev/null || true; wait 2>/dev/null || true; }
  [ -n "${ZEN_E2E_KEEP:-}" ] && { echo "kept: databases zen_e2e_$$_*, files $ROOT"; return; }
  rm -rf "$ROOT"
}
trap cleanup EXIT

[ -n "${ZEN_E2E_NO_BUILD:-}" ] || cargo build --release -q
. "$REPO/scripts/test-sqlite.sh"
ensure_test_sqlite

# A git workspace with one commit, so verification has a diff baseline.
new_workspace() {
  local ws="$TMP/ws-$1"
  mkdir -p "$ws" && (cd "$ws" && git init -q && echo hello > README && git add . && git -c user.email=e2e@zen -c user.name=e2e commit -qm init)
  echo "$ws"
}

# start_kernel <workspace> <faux script> [ENV=VALUE …]
start_kernel() {
  local ws=$1 script=$2; shift 2
  (
    # The scheduler is off unless a scenario turns it on (ZEN_JOBS=1).
    export ZEN_JOBS=0 ZEN_TOKEN=$TOKEN ZEN_PORT=$PORT ZEN_WORKERS=engine ZEN_FAUX=1 ZEN_FAUX_SCRIPT=$script ZEN_WORKSPACE=$ws \
      ZEN_ENGINE_CMD="$BIN/zen-engine" DATABASE_URL="$DB_URL" HOME="$TMP/home"
    unset ZEN_S1_MODEL OPENROUTER_API_KEY ZEN_TZ # the owner's settings stay out when run from a session
    for kv in "$@"; do export "$kv"; done
    mkdir -p "$HOME"
    # The shipped roles without their default model (a claude one), so a subagent given no model
    # runs on its parent's faux one.
    local roles="$HOME/.zenbot/agents/zenbot/roles.toml"
    [ -e "$roles" ] || { mkdir -p "${roles%/*}"; grep -v '^default_model' "$REPO/crates/zend/defaults/agent/roles.toml" >"$roles"; }
    exec "$BIN/zend"
  ) >"$TMP/kernel.log" 2>&1 &
  KERNEL_PID=$!
  wait_healthy "$URL/health" 15 "$KERNEL_PID" && return 0
  echo "kernel did not start:"; tail -20 "$TMP/kernel.log"; return 1
}

zen() { ZEN_URL=$URL ZEN_TOKEN=$TOKEN timeout 120 "$BIN/zen" "$@"; }

# A faux script with placeholders filled in: script <file> [SED-EXPR …]
script() { local f=$1; shift; local out="$TMP/$(basename "$f" .json)-$RANDOM.json"; sed "${@/#/-e}" "scripts/e2e/$f" >"$out" 2>/dev/null || cp "scripts/e2e/$f" "$out"; echo "$out"; }

check() { # check <description> <command…>
  local what=$1; shift
  if "$@"; then echo "  ok    $what"; else echo "  FAIL  $what"; FAILED=$((FAILED + 1)); return 0; fi
}
# until_ok <command…>: wait up to 10 s for a command to succeed (background work in the kernel).
until_ok() { local i; for i in $(seq 1 40); do "$@" && return 0; sleep 0.25; done; return 1; }
eq() { [ "$1" = "$2" ] || { echo "        expected [$2], got [$1]" >&2; return 1; }; }
# poll <query> <expected>: wait up to 30 s for a query's result (background work in the kernel).
poll() { local i; for i in $(seq 1 60); do [ "$(q "$1")" = "$2" ] && return 0; sleep 0.5; done; echo "        waited for [$2], got [$(q "$1")]" >&2; return 1; }

# Append a message to a session's tape as tape::append does, a minute old (so the indexer takes it now).
append_msg() { python3 "$REPO/scripts/e2e/tape.py" append "${DB_URL#sqlite://}" "$1" "$2"; }
tape_is_sound() { python3 "$REPO/scripts/e2e/tape.py" verify "${DB_URL#sqlite://}"; }

# One scenario, in a subshell of its own: its database, files and ports, its parts in order (each
# on a fresh kernel), then the tape check. Writes its output to $ROOT/<n>.out and, when done, its
# failed-check count and seconds to $ROOT/<n>.done.
in_scenario() { # in_scenario <n> <name> <function…>
  local n=$1 name=$2; shift 2
  FAILED=0  # don't inherit failed counts already printed by the parent
  PORT=$((BASE_PORT + 20 * n)) URL="http://127.0.0.1:$((BASE_PORT + 20 * n))"
  DB=zen_e2e_$$_$n TMP=$ROOT/$n
  export ZEN_DB_DIR="$TMP/databases"
  DB_URL=$(db_url_for "$DB")
  mkdir -p "$TMP"
  trap 'stop_kernel; [ -n "${ZEN_E2E_KEEP:-}" ] || { db_drop "$DB" >/dev/null 2>&1 || true; rm -rf "$TMP"; }' EXIT
  trap 'exit 143' TERM INT
  local t0=$SECONDS part
  echo "== $name"
  db_create "$DB" >/dev/null
  for part in "$@"; do
    "$part" || { echo "  FAIL  $part errored"; FAILED=$((FAILED + 1)); }
    stop_kernel
  done
  check "every tape is numbered and its hash chain recomputes" tape_is_sound
  [ "$FAILED" = 0 ] || { echo "  (kernel log: $TMP/kernel.log)"; cp "$TMP/kernel.log" "$ROOT/kernel-$name.log" 2>/dev/null || true; }
  echo "$FAILED $((SECONDS - t0)) $name" >"$ROOT/$n.done"
}

SCENARIOS=()
run() { # run <name> <function…>: register a scenario (its parts share a setup)
  [ -z "$FILTER" ] || [[ "$*" == *"$FILTER"* ]] || return 0
  SCENARIOS+=("$*")
}

# ---------- scenarios ----------

# The open loop: one default faux turn runs the bash tool and answers; a session's messages page by
# `last` and `after`, and a page says how many messages there are in all.
open_loop() {
  local ws; ws=$(new_workspace open)
  start_kernel "$ws" ""
  local r; r=$(zen ask --json -m faux/smoke "run the smoke command")
  check "answer" eq "$(echo "$r" | jq -r .error)" null
  check "the bash tool ran" eq "$(echo "$r" | jq -r '[.tools[].name] | join(",")')" bash
  local sid; sid=$(echo "$r" | jq -r .session_id)
  local all; all=$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid" | jq '.messages | length')
  local last; last=$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid" | jq '.messages[-1].seq')
  local first; first=$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid" | jq '.messages[0].seq')
  check "a session's messages page by last and after" bash -c '[ "$1" -gt 2 ] && [ "$2" = 1 ] && [ "$3" = 0 ] && [ "$4" = "$5" ]' _ "$all" \
    "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid?last=1" | jq '.messages | length')" \
    "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid?after=$last" | jq '.messages | length')" \
    "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid?after=$first" | jq '.messages | length')" "$((all - 1))"
  check "a page says how many messages there are in all" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid?last=1" | jq .message_count)" "$all"
}

# The prompt files: installed when missing, never overwritten, read at session start.
prompt_files() {
  local ws; ws=$(new_workspace prompt)
  mkdir -p "$TMP/home/.zenbot" && echo "# USER.md
The owner's name is E2E Owner." >"$TMP/home/.zenbot/USER.md"
  start_kernel "$ws" ""
  local zh="$TMP/home/.zenbot"
  check "defaults installed, scoped" test -s "$zh/agents/zenbot/SOUL.md" -a -s "$zh/agents/zenbot/IDENTITY.md" -a -s "$zh/AGENTS.md" -a -s "$zh/global/skills/work/verify/SKILL.md" -a -s "$zh/global/skills/work/brief/references/template.md"
  check "a fresh home has no old-layout paths" test ! -e "$zh/SOUL.md" -a ! -e "$zh/skills"
  check "the owner's USER.md was kept" grep -q "E2E Owner" "$zh/USER.md"
  local sid; sid=$(zen ask --json -m faux/smoke "hi" | jq -r .session_id)
  local base; base=$(q "SELECT payload->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='base'")
  check "SOUL.md is in the instructions, from the agent's folder" grep -qF '<soul file="~/.zenbot/agents/zenbot/SOUL.md">' <<<"$base"
  check "IDENTITY.md follows the soul" bash -c 'grep -A2 -F "</soul>" <<<"$1" | grep -qF "<identity file=\"~/.zenbot/agents/zenbot/IDENTITY.md\">"' _ "$base"
  check "AGENTS.md is the environment, placeholders filled" grep -qF "Working directory for tools: $ws" <<<"$base"
  check "USER.md is in the instructions" grep -q "E2E Owner" <<<"$base"
  check "the skills index is in the instructions" grep -q "work/verify:" <<<"$base"
  check "memory starts empty" grep -q "(empty)" <<<"$base"
  local tools; tools=$(q "SELECT group_concat(t.value->>'name', ',') FROM turns, envelopes e, json_each(e.tools) t WHERE turns.session_id=X'${sid//-/}' AND e.hash = turns.envelope")
  check "the system tools, in order, without the retired work tool" eq "$tools" bash,read,write,edit,history,search,ask,remember,brain,web_search,web_fetch,find_skills,load_skill,save_skill,find_tools,load_tool,call_tool,verify,delegate,schedule
}

# A home in the old flat layout moves into the scoped one (layout.rs) when the kernel starts, once:
# the owner's files keep their content and history, nothing is overwritten by a default, and a
# symlink at each old path still leads to the file (what a rolled-back build reads).
layout_move() {
  local ws; ws=$(new_workspace layout)
  local zh="$TMP/old-home"
  mkdir -p "$zh/wiki" "$zh/skills/work/verify"
  printf '# SOUL.md\nI am the E2E soul, moved.\n' >"$zh/SOUL.md"
  echo "old memory copy" >"$zh/MEMORY.md"
  echo "my own verify" >"$zh/skills/work/verify/SKILL.md"
  (cd "$zh/wiki" && git init -q && echo "# A page" >page.md && git add . && git -c user.email=e2e@zen -c user.name=e2e commit -qm "a page")
  start_kernel "$ws" "" ZEN_HOME="$zh"
  check "moved to the scoped layout" test -s "$zh/agents/zenbot/SOUL.md" -a -d "$zh/global/wiki/.git" -a -d "$zh/global/skills/work/verify"
  check "the soul kept its content" grep -q "E2E soul, moved" "$zh/agents/zenbot/SOUL.md"
  check "the wiki kept its history" eq "$(git -C "$zh/global/wiki" log --format=%s)" "a page"
  check "the agent's own skill wasn't overwritten" eq "$(cat "$zh/global/skills/work/verify/SKILL.md")" "my own verify"
  check "missing defaults went to the new layout" test -s "$zh/global/skills/work/brief/SKILL.md"
  check "old paths are symlinks to the new ones" eq "$(readlink "$zh/SOUL.md") $(readlink "$zh/wiki") $(readlink "$zh/skills") $(readlink "$zh/MEMORY.md")" \
    "agents/zenbot/SOUL.md global/wiki global/skills global/MEMORY.md"
  check "and still read through" grep -q "E2E soul, moved" "$zh/SOUL.md"
  local sid; sid=$(zen ask --json -m faux/smoke "hi" | jq -r .session_id)
  check "the moved soul is in the instructions" grep -q "E2E soul, moved" <<<"$(q "SELECT payload->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='base'")"
  stop_kernel
  start_kernel "$ws" "" ZEN_HOME="$zh"
  check "a second start changes nothing" eq "$(grep -c 'moved ' "$TMP/kernel.log") $(readlink "$zh/SOUL.md") $(cat "$zh/agents/zenbot/SOUL.md" | tail -1)" \
    "0 agents/zenbot/SOUL.md I am the E2E soul, moved."
}

# Agents (agents.rs): the owner makes one; its sessions get its soul and only its tools, the kernel
# refuses anything else, it can't widen its own settings, and a job it schedules runs as itself.
agents() {
  local ws; ws=$(new_workspace agents)
  local zh="$TMP/home/.zenbot"
  start_kernel "$ws" "$(script agents.json "s|ZENHOME|$zh|")" ZEN_DEFAULT_MODEL=faux/smoke
  check "zen agents new makes one" grep -q "Made agent \`garry\`" <<<"$(zen agents new garry Runs ideas through YC frameworks)"
  check "its starting soul names it" grep -q "You are Garry" "$zh/agents/garry/SOUL.md"
  check "a second one with that name is refused" bash -c '! "$@" >/dev/null 2>&1' _ env ZEN_URL="$URL" ZEN_TOKEN="$TOKEN" "$BIN/zen" agents new garry
  cat >"$zh/agents/garry/agent.toml" <<TOML
description = "Runs ideas through YC frameworks"
tools = ["read", "history", "search", "ask", "find_tools", "schedule"]
mcp = []
TOML
  check "zen agents lists it with its tools" grep -q "garry.*tools: read, history, search, ask, find_tools, schedule; no MCP" <<<"$(zen agents)"
  local r sid; r=$(zen ask --json --agent garry -m faux/smoke "garry try your tools"); sid=$(echo "$r" | jq -r .session_id)
  check "the session is garry's" eq "$(q "SELECT agent FROM sessions WHERE id=X'${sid//-/}'")" garry
  local tools; tools=$(q "SELECT group_concat(t.value->>'name', ',') FROM turns, envelopes e, json_each(e.tools) t WHERE turns.session_id=X'${sid//-/}' AND e.hash = turns.envelope")
  check "offered only its tools, in zenbot's order" eq "$tools" read,history,search,ask,find_tools,schedule
  check "its soul is in the instructions, not zenbot's" bash -c 'grep -q "You are Garry" <<<"$1" && grep -q "agents/garry/SOUL.md" <<<"$1"' _ "$(q "SELECT payload->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='base'")"
  check "bash refused by the kernel, read runs, no MCP server is offered" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" bash:true,read:false,find_tools:false
  check "and says why" grep -q "isn't one of garry's tools" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult' ORDER BY seq LIMIT 1")"
  check "the board shows whose session it is" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/board" | jq -r --arg id "$sid" '.sessions[] | select(.id == $id) | .agent')" garry
  r=$(zen ask --json --agent garry -m faux/smoke "schedule as zenbot")
  check "a job it schedules can't run as another agent" grep -q "runs as you (garry)" <<<"$(echo "$r" | jq -r '.tools[0].output // empty')$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'$(echo "$r" | jq -r .session_id | tr -d -)' AND payload->>'role'='toolResult'")"
  zen jobs add zen-daily -s "every 2h" -p "zenbot's own job" >/dev/null
  r=$(zen ask --json --agent garry -m faux/smoke "hijack zenbot's job")
  check "it can't change or run zenbot's job, nor make a shell job; its own job it can" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" schedule:true,schedule:true,schedule:true,schedule:false,schedule:false
  local why; why=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'$(echo "$r" | jq -r .session_id | tr -d -)' AND payload->>'role'='toolResult'")
  check "each refusal says why" bash -c '[ "$(grep -o "is zenbot.s job, not yours (garry)" <<<"$1" | wc -l)" = 2 ] && grep -q "bash. isn.t one of garry.s tools" <<<"$1"' _ "$why"
  check "zenbot's job is unchanged" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "zen-daily") | .prompt')" "zenbot's own job"
  check "its own job runs as garry" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "garry-own") | "\(.agent)|\(.prompt)"')" "garry|review them harder"
  r=$(zen ask --json -m faux/smoke "widen garry")
  check "no agent can change an agent's settings; a soul edit is backed up" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" write:true,edit:false
  check "the settings are unchanged" grep -q 'tools = \["read"' "$zh/agents/garry/agent.toml"
  # zenbot hands a task to garry: the child is garry's session, in its workspace (the kernel's here).
  r=$(zen ask --json -m faux/smoke "hand garry a task"); local zsid; zsid=$(echo "$r" | jq -r .session_id)
  check "zenbot delegates to garry; an unknown agent is refused" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" delegate:false,delegate:true
  check "the child is garry's subagent, labelled so" eq "$(q "SELECT agent || ',' || kind FROM sessions WHERE parent=X'${zsid//-/}'")|$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${zsid//-/}' AND payload->>'toolName'='delegate' AND payload->'content'->0->>'text' LIKE '%(garry) on faux/smoke%'")" "garry,subagent|1"
  check "a session with an unknown agent is refused" eq "$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"agent":"nobody"}' "$URL/api/sessions")" 400
  # The owner's apps read, edit and remove agents through the API.
  api() { curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
  local g; g=$(api "$URL/api/agents/garry")
  check "an agent's files and sessions" eq "$(echo "$g" | jq -r '"\(.soul | test("You are Garry"))|\(.settings | test("tools"))|\(.sessions)"')" "true|true|3"
  check "bad settings are refused" eq "$(api -o /dev/null -w '%{http_code}' -X PUT -d '{"soul":"x","settings":"tools = [\"rm\"]"}' "$URL/api/agents/garry")" 400
  check "and nothing is written" grep -q "You are Garry" "$zh/agents/garry/SOUL.md"
  check "good ones are saved" eq "$(api -X PUT -d '{"settings":"tools = [\"read\", \"search\"]\n"}' "$URL/api/agents/garry" | jq -c .tools)" '["read","search"]'
  check "zenbot can't be removed" eq "$(api -o /dev/null -w '%{http_code}' -X DELETE "$URL/api/agents/zenbot")" 400
  zen agents new scratch >/dev/null
  check "agents rm removes one, kept in backups" bash -c 'grep -q "Removed agent .scratch." <<<"$1" && ls "$2"/backups/agents/scratch-*/SOUL.md >/dev/null' _ "$(zen agents rm scratch)" "$zh"
  echo 'tools = ["rm"]' >"$zh/agents/garry/agent.toml"
  r=$(zen ask --json -s "$sid" "garry again" 2>&1 || true)
  check "a broken agent.toml stops the turn instead of running wider" grep -q "unknown tool" <<<"$r"
  check "and nothing ran" eq "$(q "SELECT count(*) FROM tool_calls WHERE session_id=X'${sid//-/}'")" 3
  check "zen agents says what's broken" grep -q "garry.*BROKEN: .*unknown tool" <<<"$(zen agents)"
}

# Skills load on demand, as tool results; nothing outside a skill's folder can be read through them.
skills_on_demand() {
  local ws; ws=$(new_workspace skills)
  start_kernel "$ws" "$(script agent.json)"
  local r sid; r=$(zen ask --json -m faux/smoke "use a skill"); sid=$(echo "$r" | jq -r .session_id)
  check "find, load, load a file, refused outside" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" find_skills:false,load_skill:false,load_skill:false,load_skill:true
  local res; res=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")
  check "find_skills names it" grep -q "work/verify:" <<<"$res"
  check "load_skill returns SKILL.md" grep -q "Decide whether a fresh verifier adds something" <<<"$res"
  check "and a reference file" grep -q "## Criteria" <<<"$res"
  check "the instructions didn't change mid-session" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='envelope'")" 1
}

# MCP: tools from a local (stdio) and a remote (HTTP) server, found, loaded and called through the
# three fixed tools; a call missing a required argument is refused by the kernel; the kernel's
# token isn't filled into a header for a server that doesn't list it in `allow_env`. Starts mcp_server.py (stdio, and `--http` on the e2e port + 1).
mcp_tools() {
  local ws; ws=$(new_workspace mcp)
  local port=$((PORT + 1))
  python3 "$REPO/scripts/e2e/mcp_server.py" --http "$port" & local srv=$!
  mkdir -p "$TMP/home/.zenbot"
  cat >"$TMP/home/.zenbot/mcp.json" <<JSON
{ "mcpServers": {
  "local": { "command": "python3", "args": ["$REPO/scripts/e2e/mcp_server.py", "--child-pidfile", "$TMP/home/.cache/zenbot/mcp/local/mcp-child.pid"] },
  "remote": { "url": "http://127.0.0.1:$port/mcp", "headers": { "Authorization": "Bearer \${ZEN_E2E_MCP_TOKEN}" }, "allow_env": ["ZEN_E2E_MCP_TOKEN"] },
  "greedy": { "url": "http://127.0.0.1:$port/mcp", "headers": { "Authorization": "Bearer \${ZEN_TOKEN}" }, "exclude": ["echo", "add"] } } }
JSON
  start_kernel "$ws" "$(script reach.json)" ZEN_E2E_MCP_TOKEN=e2e
  check "MCP status does not start configured stdio servers" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/mcp" | jq '.servers.local')/$(test -e "$TMP/home/.cache/zenbot/mcp/local/mcp-child.pid" && echo started || echo idle)" "0/idle"
  local r sid; r=$(zen ask --json -m faux/smoke "mcp please"); sid=$(echo "$r" | jq -r .session_id)
  check "MCP calls run, argument validation applies" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" \
    find_tools:false,load_tool:false,call_tool:false,call_tool:false,call_tool:true
  local res; res=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")
  check "find_tools lists namespaced tools" grep -q "local_echo: Echo a message back." <<<"$res"
  check "load_tool shows the schema" grep -q '"required"' <<<"$res"
  check "the local server answered" grep -q "echo: hi" <<<"$res"
  check "the remote server answered (event stream)" grep -qE '(^|\|)5(\||$)' <<<"$res"
  check "a missing argument is named" grep -q "missing required arguments: b" <<<"$res"
  check "the kernel's token is filled in only where allow_env lists it" grep -q 'server `greedy`: environment variables left empty: ZEN_TOKEN (a secret' <<<"$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/mcp" | jq -r '.problems[]')"
  check "the tool list is still fixed" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='envelope'")" 1
  # One slow call holds up neither the same server's other calls nor the catalog.
  zen ask --json -m faux/smoke "tool slow" >"$TMP/mcp-slow.json" & local slow=$!
  sleep 1.5
  local t0=$SECONDS; r=$(zen ask --json -m faux/smoke "tool quick")
  check "a call runs while another is slow" bash -c '[ "$1" -lt 5 ] && grep -q "echo: quick" <<<"$2"' _ "$((SECONDS - t0))" \
    "$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'$(echo "$r" | jq -r .session_id | tr -d -)' AND payload->>'role'='toolResult'")"
  wait "$slow"
  check "and the slow one finishes" grep -q "slow: done" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'$(jq -r .session_id "$TMP/mcp-slow.json" | tr -d -)' AND payload->>'role'='toolResult'")"
  r=$(zen ask --json -m faux/smoke "tool huge")
  check "an answer over 8 MB is an error, stdio and HTTP" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" call_tool:true,call_tool:true
  check "both oversized results are refused" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'$(echo "$r" | jq -r .session_id | tr -d -)' AND payload->>'role'='toolResult' AND payload LIKE '%over 8 MB%'")" 2
  # A server's own children go with it: changing mcp.json restarts the servers.
  local marker="$TMP/home/.cache/zenbot/mcp/local/mcp-child.pid"
  local heartbeat="$marker.$(cat "$marker")"
  local beat; beat=$(cat "$heartbeat"); sleep .3
  check "the local server's child runs" test "$beat" != "$(cat "$heartbeat")"
  sed -i 's/"exclude": \["echo", "add"\]/"exclude": ["echo"]/' "$TMP/home/.zenbot/mcp.json"
  zen ask --json -m faux/smoke "tool find" >/dev/null
  sleep .5; beat=$(cat "$heartbeat"); sleep .3
  check "and stops when the server is replaced" eq "$(cat "$heartbeat")" "$beat"
  kill "$srv" 2>/dev/null || true
}

# Script jobs: a command run with no model, live at once whether the owner or the agent
# made it. Each run records its command and exit code; exit 0 with no output is silent, any other
# exit an error; secrets in its output are masked.
script_jobs() {
  local ws; ws=$(new_workspace script-jobs)
  start_kernel "$ws" "$(script jobs.json)" ZEN_JOBS=1 ZEN_JOBS_TICK=0.5 ZEN_DEFAULT_MODEL=faux/smoke
  local runs="SELECT group_concat(r.status, ',' ORDER BY r.id) FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name ="
  zen jobs add sj-ok --schedule "every 1h" --command 'echo hello from a script; echo line two' >/dev/null
  zen jobs run sj-ok >/dev/null
  check "an owner's script job runs" poll "$runs 'sj-ok'" ok
  check "its output is the report" grep -q "hello from a script" <<<"$(zen jobs runs sj-ok)"
  check "the run records its command and exit code" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/jobs/runs?job=sj-ok" | jq -r '.[0] | "\(.command)|\(.exit_code)"')" "echo hello from a script; echo line two|0"
  check "and no model ran" eq "$(q "SELECT count(*) FROM turns t JOIN job_runs r ON r.session_id = t.session_id JOIN jobs j ON j.id = r.job_id WHERE j.name = 'sj-ok'")" 0
  zen jobs add sj-fail --schedule "every 1h" --command 'echo about to fail; exit 3' >/dev/null
  zen jobs run sj-fail >/dev/null
  check "a non-zero exit is an error" poll "$runs 'sj-fail'" error
  check "with its exit code" grep -q "exit code 3" <<<"$(q "SELECT error FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name = 'sj-fail'")"
  zen jobs add sj-quiet --schedule "every 1h" --command 'true' >/dev/null
  zen jobs run sj-quiet >/dev/null
  check "no output is silent" poll "$runs 'sj-quiet'" silent
  zen jobs add sj-secret --schedule "every 1h" --command "cat $TMP/home/.zenbot/token; echo done" >/dev/null
  zen jobs run sj-secret >/dev/null
  poll "$runs 'sj-secret'" ok >/dev/null || poll "$runs 'sj-secret'" error >/dev/null
  check "the kernel's token is masked in the output" bash -c '! grep -q "$1" <<<"$2"' _ "$TOKEN" "$(q "SELECT coalesce(output,'') || coalesce(error,'') FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name = 'sj-secret'")"
  zen ask --json -m faux/smoke "script please" >/dev/null
  check "a script job the agent writes is live at once" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "agent-script") | "\(.enabled)|\(.kind)"')" "true|script"
  zen jobs run agent-script >/dev/null
  check "and runs" poll "$runs 'agent-script'" ok
  zen ask --json -m faux/smoke "change script" >/dev/null
  check "changed by the agent, it stays live" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "agent-script") | "\(.enabled)|\(.command)"')" "true|echo CHANGED"
  zen jobs >"$TMP/jobs-list.txt" 2>&1
  check "the owner sees the exact command" grep -qF "$ echo CHANGED" "$TMP/jobs-list.txt"
}

# The engine cap: with ZEN_ENGINE_CONCURRENCY=2, eight delegated subtasks all finish, but
# never more than two subagent turns run at once (they wait for a slot).
engine_cap() {
  local ws; ws=$(new_workspace engine-cap)
  start_kernel "$ws" "$(script delegate.json)" ZEN_ENGINE_CONCURRENCY=2 ZEN_DEFAULT_MODEL=faux/smoke
  local sid; sid=$(zen ask --json -m faux/smoke "fan out" | jq -r .session_id)
  local kids="SELECT t.* FROM turns t JOIN sessions s ON s.id = t.session_id WHERE s.parent = X'${sid//-/}' AND s.kind = 'subagent'"
  check "every subtask finished" eq "$(q "SELECT count(*) FROM ($kids) k WHERE outcome = 'ok'")" 8
  check "never more than two subagent turns at once" eq "$(q "SELECT max(n) FROM (SELECT count(b.id) AS n FROM ($kids) a JOIN ($kids) b ON b.started_at <= a.started_at AND b.ended_at > a.started_at GROUP BY a.id) x")" 2
}

# MCP sign-in: a server behind OAuth (oauth_server.py on the e2e port + 16) asks for a
# login; the owner's login discovers the authorization server, registers zenbot, and finishes with
# the address the browser ended on; calls then carry a token that refreshes and rotates by itself.
mcp_login() {
  local ws; ws=$(new_workspace mcp-login)
  local port=$((PORT + 16))
  python3 "$REPO/scripts/e2e/oauth_server.py" "$port" & local srv=$!
  mkdir -p "$TMP/home/.zenbot"
  cat >"$TMP/home/.zenbot/mcp.json" <<JSON
{ "mcpServers": { "linear": { "url": "http://127.0.0.1:$port/mcp", "untrusted": false } } }
JSON
  start_kernel "$ws" "$(script mcp-login.json)"
  until_ok curl -sf "http://127.0.0.1:$port/stats" -o /dev/null
  api() { curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
  local results="SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE payload->>'role'='toolResult' AND session_id="
  local r; r=$(zen ask --json -m faux/smoke "call linear")
  check "before signing in, a call says how to sign in" grep -q 'zen mcp login linear' <<<"$(q "${results}X'$(jq -r .session_id <<<"$r" | tr -d -)'")"
  local start back
  start=$(api -d '{"server":"linear"}' "$URL/api/mcp/login")
  check "a login starts with the sign-in host and a PKCE link for this server" bash -c 'jq -e ".auth_server==\"127.0.0.1\" and (.authorize_url|test(\"code_challenge_method=S256\")) and (.authorize_url|test(\"resource=\"))" <<<"$1" >/dev/null' _ "$start"
  back=$(curl -s -o /dev/null -w '%{redirect_url}' "$(jq -r .authorize_url <<<"$start")")
  check "an answer for another sign-in is refused" grep -q 'another sign-in' <<<"$(api -d "{\"callback\":\"${back/state=/state=x}\"}" "$URL/api/mcp/login/$(jq -r .flow <<<"$start")")"
  start=$(api -d '{"server":"linear"}' "$URL/api/mcp/login")
  back=$(curl -s -o /dev/null -w '%{redirect_url}' "$(jq -r .authorize_url <<<"$start")")
  check "the browser is sent back to the loopback redirect, with the issuer" grep -q '^http://127.0.0.1:33418/callback?code=.*iss=' <<<"$back"
  check "pasting that address signs in, renewable" eq "$(api -d "{\"callback\":\"$back\"}" "$URL/api/mcp/login/$(jq -r .flow <<<"$start")" | jq -r '.renews')" true
  check "the credentials are private" eq "$(stat -c %a "$TMP/home/.zenbot/mcp-auth/linear.json")" 600
  check "status shows the server signed in" eq "$(api "$URL/api/mcp" | jq -r '.login.linear')" "signed in"
  r=$(zen ask --json -m faux/smoke "call linear")
  check "calls carry a token that refreshes and rotates" bash -c 'grep -qE "token [0-9]+\|token [0-9]+" <<<"$1" && [ "$(jq .refreshes <<<"$2")" -ge 2 ]' _ \
    "$(q "${results}X'$(jq -r .session_id <<<"$r" | tr -d -)'")" "$(curl -s "http://127.0.0.1:$port/stats")"
  api -d '{"server":"linear"}' "$URL/api/mcp/login" >/dev/null
  check "a new login reuses the registered client" eq "$(curl -s "http://127.0.0.1:$port/stats" | jq .registrations)" 2
  check "an unknown server can't sign in" grep -q 'no MCP server named' <<<"$(api -d '{"server":"nope"}' "$URL/api/mcp/login")"
  kill "$srv" 2>/dev/null || true
}

# Web: search results come back numbered, with non-http links dropped; `remember` with
# source=owner on words the owner didn't say saves them as inferred. Search goes through
# searxng_stub.py on the e2e port + 2 (ZEN_SEARXNG_URL).
web_tools() {
  local ws; ws=$(new_workspace web)
  local port=$((PORT + 2)) s1port=$((PORT+19))
  python3 "$REPO/scripts/e2e/systemone_stub.py" "$s1port" & local s1srv=$!
  python3 "$REPO/scripts/e2e/searxng_stub.py" "$port" & local srv=$!
  start_kernel "$ws" "$(script reach.json)" ZEN_SEARXNG_URL="http://127.0.0.1:$port" ZEN_S1_URL="http://127.0.0.1:$s1port/systemone" ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key
  local r sid; r=$(zen ask --json -m faux/smoke "web please"); sid=$(echo "$r" | jq -r .session_id)
  check "search ran, the memory was saved" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" web_search:false,remember:false
  local res; res=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")
  check "results numbered, non-http dropped" bash -c 'grep -q "\[2\] Another" <<<"$1" && ! grep -q "javascript:" <<<"$1"' _ "$res"
  check "words that aren't the owner's are saved as inferred" eq "$(q "SELECT source FROM memories WHERE text LIKE 'Something read on the web.%'")" inferred
  q "DELETE FROM memories; DELETE FROM sqlite_sequence WHERE name='memories'" >/dev/null  # the memory scenarios start from none
  kill "$srv" "$s1srv" 2>/dev/null || true
  wait "$s1srv" 2>/dev/null || true
}

# Search: a fact from one session is found from another (by words, and by its exact path first), a
# memory is found and counted as used, history reads the other session, and every search is logged.
search_recall() {
  local ws; ws=$(new_workspace search)
  start_kernel "$ws" "$(script search.json)"
  q "DELETE FROM memories; DELETE FROM sqlite_sequence WHERE name='memories'" >/dev/null
  local a; a=$(zen ask --json -m faux/smoke "save: the purple elephant config lives in deploy/elephant.yaml" | jq -r .session_id)
  # The indexer takes events once they're 5 seconds old (search.rs): age them instead of waiting.
  q "UPDATE tape_events SET created_at = strftime('%Y-%m-%dT%H:%M:%f+00:00',created_at,'-10 seconds') WHERE session_id = X'${a//-/}'" >/dev/null
  local s="$TMP/search-filled.json"; sed "s/SESSION/${a:0:8}/" "$REPO/scripts/e2e/search.json" >"$s"
  stop_kernel; start_kernel "$ws" "$s"
  local r sid; r=$(zen ask --json -m faux/smoke "find it"); sid=$(echo "$r" | jq -r .session_id)
  check "searches and history ran" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" search:false,search:false,search:false,history:false
  local res; res=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")
  check "found by words, in the other session" grep -q "session ${a:0:8}" <<<"$res"
  check "the exact path comes first" grep -q "(exact match)" <<<"$res"
  check "the memory is found" grep -q "m1 — short-term memory (inferred)" <<<"$res"
  check "and counted as used" eq "$(q "SELECT uses FROM memories WHERE id = 1")" 1
  check "history read the other session" grep -q "purple elephant" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='history'")"
  check "every search is logged" eq "$(q "SELECT count(*) FROM searches WHERE session_id=X'${sid//-/}'")" 3
  q "DELETE FROM memories; DELETE FROM sqlite_sequence WHERE name='memories'; DELETE FROM search_docs WHERE kind='memory'" >/dev/null
}

# The search index (with an embeddings stub): memories are indexed from a queue, so ones with old
# times and ones a long transaction changed are not missed; a document the provider rejects is tried
# alone 3 times while the rest are embedded; a kernel prompt doesn't split its turn; a search doesn't
# wait for the embeddings provider; and each arm filtering `search_docs` itself ranks a fixture as
# the independent SQLite oracle does (search_reference.py). Each fixture document has its own number of `filler`
# words, so no two are the same distance from a query (ties in the vector arm come in any order).
search_index() {
  local ws; ws=$(new_workspace search-index)
  local port=$((PORT + 7)) release="$TMP/embed-release"; rm -f "$release"
  python3 "$REPO/scripts/e2e/embed_stub.py" "$port" "$release" & local srv=$!
  start_kernel "$ws" "$(script search-index.json)" OPENROUTER_API_KEY=e2e-key ZEN_EMBED_URL="http://127.0.0.1:$port" ZEN_INDEX_SECS=1
  q "DELETE FROM memories; INSERT OR REPLACE INTO sqlite_sequence(name,seq) VALUES('memories',700); DELETE FROM search_docs" >/dev/null
  q "INSERT INTO memories (text, source, created_at, updated_at)
       SELECT CASE g % 6 WHEN 0 THEN 'the lighthouse keeper logs the harbour lights, entry ' || g
                         WHEN 1 THEN 'the keeper of the harbour wrote note ' || g
                         WHEN 2 THEN 'a car parked by the harbour, number ' || g
                         WHEN 3 THEN 'the lighthouse code is in the fixture crate, part ' || g
                         WHEN 4 THEN 'red car, blue car, car ' || g
                         ELSE 'an unrelated note about tea, ' || g END || replace(hex(zeroblob(g)), '00', ' filler'), 'owner', strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-3 days'), strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-3 days')
       FROM (WITH RECURSIVE n(g) AS (VALUES(1) UNION ALL SELECT g+1 FROM n WHERE g<36) SELECT g FROM n);
     INSERT INTO memories (text, source) VALUES ('REJECT-EMBED the lighthouse keeper, rejected', 'owner'), ('LONGTX a memory archived in a long transaction', 'owner');
     INSERT INTO search_docs (kind, ref, ident, title, body)
       SELECT 'turn', 'fixture:' || g, CASE WHEN g % 3 = 0 THEN 'crates/fixture/engine_core.rs' ELSE 'crates/fixture/engine_core_' || g || '.rs' END,
              'fixture', 'Owner: change crates/fixture/engine_core_' || g || '.rs near the lighthouse harbour' || replace(hex(zeroblob(g)), '00', ' filler')
       FROM (WITH RECURSIVE n(g) AS (VALUES(1) UNION ALL SELECT g+1 FROM n WHERE g<12) SELECT g FROM n)" >/dev/null
  check "memories with old times are indexed, the rest embedded, a rejected one alone 3 times" poll \
    "SELECT count(*) FILTER (WHERE kind = 'memory') || '/' || count(*) FILTER (WHERE embedding IS NOT NULL) || '/' || group_concat(embed_attempts || ':' || (embed_error LIKE '%input rejected by the e2e stub%'), ',') FILTER (WHERE embedding IS NULL) FROM search_docs" \
    "38/49/3:1"
  # A long transaction archives a memory while a competing writer waits and then adds another.
  python3 "$REPO/scripts/e2e/long_transaction.py" "${DB_URL#sqlite://}" >/dev/null & local longtx=$!
  sleep 1
  q "INSERT INTO memories (text, source) VALUES ('AFTER-LONGTX committed after lock release', 'owner')" >/dev/null
  check "a competing writer is indexed after the lock releases" poll "SELECT count(*) FROM search_docs WHERE body LIKE 'AFTER-LONGTX%'" 1
  wait "$longtx"
  check "and the long transaction's archive is too" poll "SELECT count(*) FROM search_docs WHERE body LIKE 'LONGTX%'" 0
  # A turn a kernel prompt continues stays one document when the indexer reads it in two passes.
  local ks; ks=$(q "INSERT INTO sessions (id, title, model) VALUES (randomblob(16), 'kernel split', 'faux/smoke') RETURNING id")
  append_msg "$ks" '{"role": "user", "content": "OWNER-ASKS for the kernel split"}'
  append_msg "$ks" '{"role": "assistant", "content": [{"type": "text", "text": "working on it"}]}'
  append_msg "$ks" '{"role": "user", "kernel": true, "content": "KERNEL-CONTINUES"}'
  check "the turn is indexed" poll "SELECT count(*) FROM search_docs WHERE session_id = X'${ks//-/}'" 1
  append_msg "$ks" '{"role": "assistant", "content": [{"type": "text", "text": "after the continuation"}]}'
  check "a kernel prompt doesn't split it" poll \
    "SELECT group_concat(ref || ':' || (body LIKE '%after the continuation%'), ',') FROM search_docs WHERE session_id = X'${ks//-/}'" "$ks:1:1"
  check "the tape stays sound" tape_is_sound
  # A search while the provider holds an embedding batch.
  q "INSERT INTO memories (text, source) VALUES ('SLOW-EMBED the provider takes its time', 'owner')" >/dev/null
  local i held=0; for i in $(seq 1 100); do held=$(curl -s "http://127.0.0.1:$port/stats" | jq -r .held); [ "$held" = 1 ] && break; sleep 0.2; done
  local r; r=$(zen ask --json -m faux/smoke "quick search")
  check "a search runs while an embedding batch is held" bash -c '[ "$1" = 1 ] && [ "$2" = search:false ] && [ "$3" = 1 ]' _ "$held" \
    "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" "$(curl -s "http://127.0.0.1:$port/stats" | jq -r .held)"
  touch "$release"
  local qs; qs=$(echo "$r" | jq -r .session_id)
  check "everything is embedded once the turn ended" poll \
    "SELECT count(*) FROM search_docs WHERE embedding IS NULL AND embed_attempts < 3 OR session_id = X'${qs//-/}' AND embedding IS NOT NULL" 1
  # The same ranking as the query before each arm filtered the table itself.
  local query kinds expected=() want
  for query in "crates/fixture/engine_core.rs|{turn,memory,wiki}|crates/fixture/engine_core.rs" "lighthouse keeper harbour|{memory}|" "automobile|{memory}|"; do
    IFS='|' read -r want kinds exact <<<"$query"
    local vec; vec=$(curl -s -H "Authorization: Bearer e2e-key" -d "{\"input\": [\"$want\"], \"dimensions\": 1536}" "http://127.0.0.1:$port/embeddings" | jq -c '.data[0].embedding')
    expected+=("$(python3 "$REPO/scripts/e2e/search_reference.py" "${DB_URL#sqlite://}" "$want" "$vec" "$exact" "$kinds")")
  done
  local sid; sid=$(zen ask --json -m faux/smoke "compare the rankings" | jq -r .session_id)
  local got; got=$(q "SELECT group_concat((SELECT group_concat(x.value->>'ref', ',' ORDER BY CAST(x.key AS INTEGER)) FROM json_each(results) x), '|' ORDER BY id) FROM searches WHERE session_id = X'${sid//-/}'")
  check "the exact arm, words and meaning match the independent SQLite oracle" eq "$got" "$(IFS='|'; echo "${expected[*]}")"
  check "meaning finds the car notes" bash -c 'q=$(cut -d"|" -f3 <<<"$1"); [ -n "$q" ] && [ "$q" != "$(cut -d"|" -f2 <<<"$1")" ]' _ "$got"
  kill "$srv" 2>/dev/null || true
  q "DELETE FROM memories; DELETE FROM sqlite_sequence WHERE name='memories'; DELETE FROM search_docs" >/dev/null
}

# The owner's wiki: a page named by its title (spaces, an apostrophe, block aliases, text after a
# horizontal rule) is searched by its whole body and points at its file; a new knowledge record is
# refused (knowledge lives in pages), an old one is still read; retired tools stay refused.
wiki_pages() {
  local ws; ws=$(new_workspace wiki)
  local wiki="$TMP/home/.zenbot/global/wiki"
  mkdir -p "$wiki/claims"
  printf -- "---\ntype: primitive\naliases:\n  - Bayes rule\n---\n# Bayes' theorem\n\nPrior times likelihood.\n\n---\n\nTAILMARK after the rule.\n" >"$wiki/Bayes' theorem.md"
  printf -- '---\n{"id":"old-claim","store":"knowledge","kind":"claim","title":"Old claim","metadata":{}}\n---\n\nOLDCLAIM from before the wiki.\n' >"$wiki/claims/old-claim.md"
  start_kernel "$ws" "$(script retired.json)" ZEN_LEGACY_TOOL_AUTHORING=1
  local r; r=$(zen ask --json -m faux/smoke "retired tools")
  check "retired tools and a new knowledge record are refused; search and an old record's get work" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" \
    capture:true,save_tool:true,search:false,brain:true,brain:false
  local sid; sid=$(jq -r .session_id <<<"$r")
  local text; text=$(q "SELECT group_concat(payload->>'toolName' || ': ' || (payload->'content'->0->>'text'), '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")
  check "search finds the page by text after its horizontal rule" eq "$(q "SELECT count(*) FROM searches s CROSS JOIN json_each(s.results) f WHERE s.session_id=X'${sid//-/}' AND f.value->>'ref'='Bayes'' theorem'")" 1
  check "and points at its file" grep -qF "wiki page [[Bayes' theorem]] (updated" <<<"$text"
  check "with its path" grep -qF "$wiki/Bayes' theorem.md" <<<"$text"
  check "the refusal says where knowledge goes" grep -q "knowledge lives in wiki pages now" <<<"$text"
  check "an old knowledge record is still read" grep -q "OLDCLAIM from before the wiki" <<<"$text"
  check "and no record file was written" test ! -e "$wiki/concepts/new-concept.md"
  zen jobs run wiki >/dev/null
  check "the wiki job indexes and lints records, silent with nothing wrong" poll "SELECT group_concat(r.status, ',') FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name = 'wiki'" silent
}

# Workshop: save_skill writes a live skill (a reason required, a duplicate in its domain refused,
# each save a commit in the skills folder); retired save_tool is refused.
workshop() {
  local ws; ws=$(new_workspace workshop)
  start_kernel "$ws" "$(script workshop.json)"
  local sk="$TMP/home/.zenbot/global/skills"
  local r; r=$(zen ask --json -m faux/smoke "make")
  check "skill saved, duplicate refused, reason required, new domain saved, tool retired" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" \
    save_skill:false,save_skill:true,save_skill:true,save_skill:false,save_tool:true
  local sid; sid=$(echo "$r" | jq -r .session_id)
  local res; res=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")
  check "the duplicate is told to extend the existing skill" grep -q "work/release-notes already does this job: extend it instead" <<<"$res"
  check "saved skills are live at once" test -s "$sk/work/release-notes/SKILL.md" -a -s "$sk/finance/budget-review/SKILL.md"
  r=$(zen ask --json -m faux/smoke "use")
  check "find and load a saved skill" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" find_skills:false,load_skill:false
  check "zen skills lists it" eq "$(zen skills --json | jq -r '.skills[] | select(.skill == "work/release-notes") | .status')" active
  check "every save is a commit" bash -c '[ "$(git -C "$1" log --oneline | wc -l)" -ge 2 ]' _ "$sk"
}

# Delegation: subagents run their own turns (one on a named model, one on the parent's), can't ask or
# delegate, and report back. ask with wait=false keeps the turn going, and the board says the
# session waits on the owner until they answer.
delegation() {
  local ws; ws=$(new_workspace delegate)
  start_kernel "$ws" "$(script delegate.json)"
  local r sid; sid=$(zen new --model faux/smoke "split" --json | jq -r .id)
  r=$(zen ask --json -s "$sid" "split the work")
  check "two delegations" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" delegate:false,delegate:false
  local res; res=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='delegate'")
  check "the named model, then the parent's" eq "$(grep -o 'on faux/smoke finished' <<<"$res" | wc -l)" 2
  check "the subagents' answers come back" eq "$(grep -c '^Done\.$' <<<"${res//|/$'\n'}")" 2
  check "the subagents ran their task" eq "$(q "SELECT count(*) FROM sessions WHERE parent=X'${sid//-/}' AND kind='subagent'")" 2
  local child; child=$(q "SELECT id FROM sessions WHERE parent=X'${sid//-/}' AND kind='subagent' LIMIT 1")
  local tools; tools=$(q "SELECT group_concat(t.value->>'name', ',') FROM turns, envelopes e, json_each(e.tools) t WHERE turns.session_id=X'${child//-/}' AND e.hash = turns.envelope")
  check "a subagent can't ask or delegate" bash -c '! grep -qE "(^|,)(ask|delegate)(,|$)" <<<"$1" && grep -q "save_skill" <<<"$1"' _ "$tools"
  # A role: recorded on the child, its tools narrowed (the researcher can't write), its skills in
  # the brief; an unknown role is refused with the list.
  r=$(zen ask --json -m faux/smoke "role play")
  check "a role delegates; an unknown one is refused" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" delegate:false,delegate:true
  local rsid; rsid=$(echo "$r" | jq -r .session_id)
  local rchild; rchild=$(q "SELECT id FROM sessions WHERE parent=X'${rsid//-/}' AND kind='subagent'")
  check "the child records its role" eq "$(q "SELECT role FROM sessions WHERE id=X'${rchild//-/}'")" researcher
  check "the board shows its role" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/board" | jq -r --arg c "$rchild" '.sessions[] | select(.id == $c) | .role')" researcher
  check "the role's tools: no write, offered or run" eq "$(q "SELECT (SELECT count(*) FROM turns, envelopes e, json_each(e.tools) t WHERE turns.session_id=X'${rchild//-/}' AND e.hash = turns.envelope AND t.value->>'name' IN ('write','edit')) || '|' || (SELECT count(*) FROM tape_events WHERE session_id=X'${rchild//-/}' AND payload->>'toolName'='write' AND payload->'content'->0->>'text' LIKE '%researcher%role%')")" "0|1"
  check "its skill goes with the brief" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${rchild//-/}' AND kind='message' AND payload->>'role'='user' AND payload LIKE '%skill name=%work/search%'")" 1
  check "an unknown role lists the roles" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${rsid//-/}' AND payload->>'toolName'='delegate' AND payload->'content'->0->>'text' LIKE '%no role \`poet\`%coder, researcher, reviewer%'")" 1
  check "the parent's delegate lists its roles" bash -c '[ "$1" -ge 1 ]' _ "$(q "SELECT count(*) FROM turns, envelopes e, json_each(e.tools) t WHERE turns.session_id=X'${rsid//-/}' AND e.hash = turns.envelope AND t.value->>'name'='delegate' AND t.value->>'description' LIKE '%- researcher (claude/claude-haiku-5-5)%'")"
  r=$(zen ask --json -m faux/smoke "waitless")
  check "ask with wait=false keeps working" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" ask:false,bash:false
  # The sessions board: a session whose questions the owner hasn't answered is waiting on them,
  # until the owner's next message; a running subagent shows with its task under its running parent.
# Archived sessions are counted, and sent only when asked for.
  board() { curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/board"; }
  local asked; asked=$(echo "$r" | jq -r .session_id)
  waiting() { board | jq -r --arg s "$1" '.sessions[] | select(.id == $s) | .waiting'; }
  check "unanswered questions: waiting on the owner" eq "$(waiting "$asked")" true
  check "a session that asked nothing isn't" eq "$(waiting "$sid")" false
  zen ask --json -s "$asked" "b" >/dev/null
  check "the owner's answer clears it" eq "$(waiting "$asked")" false
  zen ask --json -m faux/smoke "hand off a doze" >/dev/null &
  local asker=$! seen="" i
  for i in $(seq 1 60); do
    seen=$(board | jq -r '[.sessions[] | select(.kind == "subagent" and .busy and ((.task // "") | contains("doze for a bit")))] | length')
    [ "$seen" = 1 ] && break
    sleep 0.25
  done
  check "the board shows the running subagent with its task" eq "$seen" 1
  local parent; parent=$(board | jq -r '.sessions[] | select(.kind == "subagent" and .busy) | .parent')
  check "under its running parent" eq "$(board | jq -r --arg p "$parent" '.sessions[] | select(.id == $p) | "\(.busy) \(.kind)"')" "true null"
  wait "$asker"
  check "all idle once it's done" eq "$(board | jq '[.sessions[] | select(.busy)] | length')" 0
  check "finished subagents stay listed under their parent" eq "$(board | jq --arg p "$sid" '[.sessions[] | select(.parent == $p and .kind == "subagent")] | length')" 2
  check "a session says how many children it has" eq "$(board | jq --arg p "$sid" '(.sessions[] | select(.id == $p) | .children) == ([.sessions[] | select(.parent == $p)] | length)')" true
  curl -s -X PATCH -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"archived":true}' "$URL/api/sessions/$asked" >/dev/null
  check "an archived session is counted, not sent" eq "$(board | jq -c --arg s "$asked" '[.archived >= 1, ([.sessions[] | select(.id == $s)] | length)]')" '[true,0]'
  check "until asked for" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/board?archived=true" | jq --arg s "$asked" '[.sessions[] | select(.id == $s)] | length')" 1
}

# remember: a memory saved in one session is in the next session's instructions, not the current
# one's; MEMORY.md shows it, and the owner's edits of the file are read back.
memory_across_sessions() {
  local ws; ws=$(new_workspace memory)
  start_kernel "$ws" "$(script agent.json)"
  local s1; s1=$(zen ask --json -m faux/smoke "remember this" | jq -r .session_id)
  check "saved with its source" eq "$(q "SELECT source || '/' || tier FROM memories")" inferred/short
  local file="$TMP/home/.zenbot/global/MEMORY.md"
  check "exported to MEMORY.md" grep -q "\[m1\] The owner prefers tabs" "$file"
  zen ask --json -s "$s1" "and now" >/dev/null
  check "frozen for the session that wrote it" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${s1//-/}' AND kind='base' AND payload->>'text' LIKE '%prefers tabs%'")" 0
  local s2; s2=$(zen ask --json -m faux/smoke "hello" | jq -r .session_id)
  check "in the next session's instructions" grep -q "\[m1\] The owner prefers tabs" <<<"$(q "SELECT payload->>'text' FROM tape_events WHERE session_id=X'${s2//-/}' AND kind='base'")"
  # The owner edits the file: a changed line, a new line, the old entry kept.
  sed -i 's/prefers tabs over spaces/prefers tabs, width 4/' "$file"
  echo "- The owner's new laptop is called heron." >>"$file"
  local s3; s3=$(zen ask --json -m faux/smoke "hello" | jq -r .session_id)
  local base; base=$(q "SELECT payload->>'text' FROM tape_events WHERE session_id=X'${s3//-/}' AND kind='base'")
  check "the owner's edits of MEMORY.md are read back" bash -c 'grep -q "\[m1\] The owner prefers tabs, width 4" <<<"$1" && grep -q "\[m2\] The owner.s new laptop is called heron" <<<"$1"' _ "$base"
  sed -i '/heron/d' "$file"
  zen ask --json -m faux/smoke "hello" >/dev/null
  check "a removed line archives its entry" eq "$(q "SELECT tier || '/' || reason FROM memories WHERE id = 2")" "archived/removed from MEMORY.md"
}

# Writes are never refused: past the size a sleep starts at once. Without System One the sleep keeps
# everything, deletes nothing and records what it did; the instructions show the newest entries
# that fit, and the next session gets the morning note.
memory_sleep() {
  local ws; ws=$(new_workspace sleep)
  start_kernel "$ws" "$(script agent.json)" ZEN_MEMORY_CHARS=1000
  q "DELETE FROM decisions; DELETE FROM sleep_runs; DELETE FROM memories" >/dev/null
  q "INSERT INTO memories (text, source, created_at) SELECT 'memory number ' || i || ' ' || replace(hex(zeroblob(80)), '00', 'x'), 'inferred', strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-' || (31-i) || ' hours') FROM (WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<30) SELECT i FROM n)" >/dev/null
  local r; r=$(zen ask --json -m faux/smoke "remember this")
  check "past the size, add still saves" eq "$(echo "$r" | jq -r '.tools[0].is_error')" false
  check "and starts a sleep" poll "SELECT trigger FROM sleep_runs WHERE ended_at IS NOT NULL ORDER BY id LIMIT 1" size
  r=$(zen memory sleep --json)
  check "a sleep by hand reports what it did" eq "$(echo "$r" | jq -r '.entries == .kept + .promoted + .archived + .changed')" true
  check "without System One nothing is promoted or archived" eq "$(echo "$r" | jq -r '.promoted + .archived')" 0
  check "nothing deleted" eq "$(q "SELECT count(*) FROM memories")" 31
  local s2; s2=$(zen ask --json -m faux/smoke "hello" | jq -r .session_id)
  local base; base=$(q "SELECT payload->>'text' FROM tape_events WHERE session_id=X'${s2//-/}' AND kind='base'")
  check "the instructions show the newest that fit" bash -c 'grep -q "prefers tabs" <<<"$1" && ! grep -q "memory number 1 " <<<"$1" && grep -q "older entries are left out" <<<"$1"' _ "$base"
  check "the next session hears about the sleep" grep -q "Last sleep" <<<"$base"
  check "zen memory lists it" grep -q "last sleep" <<<"$(zen memory)"
}

# ask ends the turn; tools that are gone are refused.
ask_and_gone_tools() {
  local ws; ws=$(new_workspace ask)
  start_kernel "$ws" "$(script agent.json)"
  local r sid; r=$(zen ask --json -m faux/smoke "ask me"); sid=$(echo "$r" | jq -r .session_id)
  check "the questions were recorded" eq "$(q "SELECT payload->'questions'->0->>'question' FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='questions'")" "Which flag name?"
  check "nothing runs after ask in that turn" eq "$(echo "$r" | jq -r '[.tools[].is_error] | join(",")')" false,true
  r=$(zen ask --json -m faux/smoke "move it")
  check "move is gone" eq "$(echo "$r" | jq -r '.tools[0].is_error')" true
  check "README not moved" test -e "$ws/README"
}

# verify: the kernel runs the commands, a fresh verifier (bash and read, no write) judges the rest;
# a failed command skips it.
verifier() {
  local ws; ws=$(new_workspace verifier)
  start_kernel "$ws" "$(script judgment.json 's/TARGET/done.txt/')"
  local r sid; r=$(zen ask --json -m faux/smoke "Please create done.txt"); sid=$(echo "$r" | jq -r .session_id)
  local child; child=$(q "SELECT id FROM sessions WHERE parent=X'${sid//-/}' AND kind='verifier'")
  check "a criterion needing judgment ran the verifier" test -n "$child"
  check "the verifier works in the repository" grep -q "^$ws" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${child//-/}' AND payload->>'toolName'='bash' ORDER BY seq LIMIT 1")"
  check "the verifier has no write tool" test ! -e "$ws/verifier-wrote.txt"
  check "command passed, judgment uncertain" eq "$(q "SELECT group_concat(x.value->>'result', ',') FROM tape_events, json_each(payload->'results') x WHERE session_id=X'${sid//-/}' AND kind='verification'")" pass,uncertain
  check "the result went back to the model" grep -q "1 passed, 0 failed, 1 uncertain" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='verify'")"
  # A failed command needs no verifier: its output is the evidence.
  stop_kernel
  start_kernel "$ws" "$(script judgment.json 's/TARGET/missing.txt/')"
  sid=$(zen ask --json -m faux/smoke "again" | jq -r .session_id)
  check "a failed command skips the verifier" eq "$(q "SELECT count(*) FROM sessions WHERE parent=X'${sid//-/}'")" 0
  check "and fails the criterion" eq "$(q "SELECT payload->'results'->0->>'result' FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='verification'")" fail
}

# Summaries: past the soft limit a summary is prepared in the background and applied after a pause
# (a `compaction` block, recorded as an expected `summary` cache break); it keeps the facts, and the
# history tool still finds the summarized messages.
summaries() {
  local ws; ws=$(new_workspace summary)
  start_kernel "$ws" "$(script summary.json)" ZEN_CONTEXT_TOKENS=4000 ZEN_COMPACT_IDLE_SECS=0 ZEN_SUMMARY_MODEL=faux/smoke
  local sid; sid=$(zen ask --json -m faux/smoke "one" | jq -r .session_id)
  zen ask --json -s "$sid" "two" >/dev/null; sleep 1
  for _ in $(seq 1 20); do [ "$(q "SELECT count(*) FROM compactions")" -gt 0 ] && break; sleep 0.5; done
  zen ask --json -s "$sid" "three" >/dev/null
  check "a summary was prepared and applied" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='compaction'")" 1
  check "the turn records it as an expected cache break" eq "$(q "SELECT cache_break FROM turns WHERE session_id=X'${sid//-/}' ORDER BY started_at DESC LIMIT 1")" summary
  check "the summary kept the fact" grep -q E4127 <<<"$(q "SELECT payload->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='compaction'")"
  check "the history tool finds summarized messages" grep -q "Tool result (bash): FACT: the error code is E4127" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='history' ORDER BY seq DESC LIMIT 1")"
}

# The direct System One call (systemone_stub.py on the e2e port + 3, ZEN_S1_URL): the stub checks the
# auth and model id; `decide` answers are parsed (`bool` from `noul`, choice, score) and logged. The
# sleep promotes the owner's lasting fact into USER.md under `## Learned` with a backup (an inference
# stays out); an agent's edit of USER.md is backed up and told to compact it when over its cap, and
# the sleep compacts it through a kernel session.
system_one_direct() {
  local ws; ws=$(new_workspace systemone)
  local port=$((PORT + 3))
  python3 "$REPO/scripts/e2e/systemone_stub.py" "$port" & local srv=$!
  start_kernel "$ws" "$(script systemone.json)" ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key ZEN_S1_URL="http://127.0.0.1:$port/systemone" \
    ZEN_DEFAULT_MODEL=faux/smoke ZEN_USER_CHARS=1200
  check "the kernel reports its own OpenRouter key" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/models" | jq -r .authenticated.openrouter)" true
  local sid; sid=$(zen ask --json -m faux/smoke "decide directly" | jq -r .session_id)
  local answers; answers=$(q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='judge'")
  check "judge: a yes/no answer per item, mapped from noul" bash -c 'grep -q "\[3\] yes (p yes 0.82)" <<<"$1"' _ "$answers"
  check "judge: a choice per item" grep -q '\[1\] build' <<<"$answers"
  check "every judge call is in the ledger, with its latency" eq "$(q "SELECT count(*) FILTER (WHERE latency_ms IS NOT NULL AND source='model') || '/' || count(*) FROM decisions WHERE session_id=X'${sid//-/}' AND point='judge' AND error IS NULL")" 2/2
  # The decision files: the built-in ones pass the checker; an owner file with a typo is reported with
  # its line and a suggestion, and never takes effect.
  local zs="$TMP/home/.zenbot/s1"; mkdir -p "$zs"
  check "the built-in decision files pass the checker" grep -Eq "[0-9]+ decisions in force \(0 from your files\), 0 error" <<<"$(zen s1 check)"
  sed 's/^false_no = 1$/false_noo = 1/' "$REPO/crates/zend/s1/memory_long.toml" >"$zs/memory_long.toml"
  sleep 2.5
  local report; report=$(zen s1 check 2>&1 || true)
  check "a typo is reported with its line and a suggestion" grep -q "memory_long.toml:[0-9]*:1  error  unknown field \`false_noo\`; did you mean \`false_no\`?" <<<"$report"
  check "and the built-in file stays in force" eq "$(zen s1 check --json 2>/dev/null | jq -r '.files[] | select(.name=="memory_long") | .source')" built-in
  check "zen status points at the problem" grep -q "problem(s) in the decision files" <<<"$(zen status 2>&1)"
  rm -f "$zs/memory_long.toml"
  local zh="$TMP/home/.zenbot"; printf '# USER.md\n\nE2E Owner.\n' >"$zh/USER.md"
  q "INSERT INTO memories(text,source) VALUES('PROMOTE durable owner decision','owner')" >/dev/null
  local r; r=$(zen memory sleep --json)
  check "a lasting memory moves to long-term memory" eq "$(echo "$r"|jq -r .promoted)" 1
  check "as a record file of its own, citing it" bash -c 'grep -l "PROMOTE durable owner decision" "$1"/memory/*.md | xargs grep -q "\"source\": \"m1\""' _ "$zh/global"
  check "the sleep leaves prompt files alone" bash -c '! grep -q PROMOTE "$1"' _ "$zh/USER.md"
  check "the decision is recorded" test "$(q "SELECT count(*) FROM decisions WHERE point='memory_durable' AND source='model'")" -ge 1
  # System One down: a decision fails fast and says so; nothing waits for it.
  kill "$srv" 2>/dev/null || true; wait "$srv" 2>/dev/null || true
  local t0=$SECONDS
  sid=$(zen ask --json -m faux/smoke "judge without System One" | jq -r .session_id)
  check "with System One down, judge fails within seconds" test $((SECONDS - t0)) -lt 20
  check "and says why" grep -q "judge: " <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='judge'")"
  check "the failure is in the ledger" eq "$(q "SELECT source FROM decisions WHERE session_id=X'${sid//-/}' AND point='judge'")" unavailable
}

# The sleep with System One (stub): a lasting memory moves to long-term memory, a finished one is
# archived, an open one stays; a memory changed while the sleep runs is left as it is now; past its
# size the least active leave until it fits. The stub holds the questions about a HOLD
# entry until the scenario has changed it.
memory_sleep_judged() {
  local ws; ws=$(new_workspace sleep-judged)
  local port=$((PORT + 5)) release="$TMP/sleep-release"
  python3 "$REPO/scripts/e2e/systemone_stub.py" "$port" "$release" & local srv=$!
  start_kernel "$ws" "$(script agent.json)" ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key ZEN_S1_URL="http://127.0.0.1:$port/systemone"
  local zh="$TMP/home/.zenbot"
  q "DELETE FROM memories; INSERT INTO memories(text,source) VALUES('PROMOTE we chose Postgres for its vectors','owner'),('ARCHIVE the deploy finished','inferred'),('KEEP is the pilot still open','inferred')" >/dev/null
  touch "$release"
  local r; r=$(zen memory sleep --json)
  check "lasting, finished and open memories each go their way" eq "$(q "SELECT group_concat(tier || ':' || coalesce(reason, ''), ',' ORDER BY id) FROM memories")" "archived:promoted to long-term memory $(date -u +%F)-promote-we-chose-postgres-for-its-vectors,archived:the sleep: finished,short:"
  check "the long-term memory is a file, found by search" bash -c 'test -s "$1" && grep -q "we chose Postgres" "$1"' _ "$zh/global/memory/$(date -u +%F)-promote-we-chose-postgres-for-its-vectors.md"
  check "a second sleep promotes nothing again" eq "$(zen memory sleep --json | jq -r .promoted)" 0
  rm -f "$release"
  local mid; mid=$(q "INSERT INTO memories(text,source) VALUES('HOLD PROMOTE a pending decision','inferred') RETURNING id")
  zen memory sleep --json >"$TMP/sleep-concurrent.json" & local sleeper=$!
  sleep_held() { curl -sf "http://127.0.0.1:$port/stats" | jq -e '.held>0' >/dev/null; }
  check "the sleep asks System One without holding memory" until_ok sleep_held
  q "UPDATE memories SET text='a corrected pending decision', updated_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id=$mid" >/dev/null
  touch "$release"; wait "$sleeper"
  check "a memory changed during the sleep is left as it is now" eq "$(q "SELECT text || '/' || tier FROM memories WHERE id=$mid")" 'a corrected pending decision/short'
  check "and the sleep says so" eq "$(jq -r .changed "$TMP/sleep-concurrent.json")" 1
  # Over its size with every entry still current: the oldest leave (to the archive, none is
  # lasting) until it fits with room to spare; the newest stay.
  q "DELETE FROM memories; INSERT INTO memories (text, source, created_at) SELECT 'current entry ' || i || ' ' || replace(hex(zeroblob(140)), '00', 'y'), 'inferred', strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-' || (41-i) || ' hours') FROM (WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<40) SELECT i FROM n)" >/dev/null
  r=$(zen memory sleep --json)
  check "past its size, the sleep brings memory under it" test "$(q "SELECT SUM(length(text) + 10) FROM memories WHERE tier='short'")" -le 3200
  check "the oldest leave first, archived and not deleted" eq "$(q "SELECT tier || '/' || reason FROM memories WHERE text LIKE 'current entry 1 %'")" "archived/the sleep: to fit the size"
  check "the newest stay" eq "$(q "SELECT tier FROM memories WHERE text LIKE 'current entry 40 %'")" short
  check "and the report counts them" eq "$(echo "$r" | jq -r '.fitted == .archived and .fitted > 0 and .entries == .kept + .promoted + .archived + .changed')" true
  kill "$srv"; wait "$srv" 2>/dev/null || true
}

# The file tools: the zen home's files aren't attached as project context; no OpenRouter key shows
# as not signed in. (Edit's trailing newline
# and read's 16 MiB limit are unit tests in tools.rs.)
kernel_tools() {
  local ws; ws=$(new_workspace kernel-tools)
  mkdir -p "$TMP/home/sub"
  # A 1×1 PNG for `read`, and a file for a batch `edit`.
  base64 -d >"$ws/pixel.png" <<<"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jF3sAAAAASUVORK5CYII="
  printf 'alpha\nbeta\n' >"$ws/notes.txt"
  start_kernel "$ws" "$(script kernel-tools.json "s|ZENHOME|$TMP/home/.zenbot|" "s|ZENALIAS|$TMP/home/sub/../.zenbot|")"
  local sid; sid=$(zen ask --json -m faux/smoke "check kernel tools" | jq -r .session_id)
  local image="SELECT %s FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='read' AND payload->'content'->1->>'type'='image'"
  check "an image read reaches the tape as an image block" eq "$(q "$(printf "$image" "payload->'content'->1->>'mimeType'")")" image/png
  check "a batch edit applies every replacement and shows the diff" bash -c '[ "$(cat "$1")" = "$(printf "one\ntwo")" ] && grep -q "^+one" <<<"$2"' _ "$ws/notes.txt" "$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='edit' ORDER BY seq LIMIT 1")"
  check "a batch with a missing match changes nothing" bash -c 'grep -q "no edits written" <<<"$1" && grep -q "^one$" "$2"' _ "$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='edit' ORDER BY seq DESC LIMIT 1")" "$ws/notes.txt"
  check "the zen home environment file is not attached as project context" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='context'")" 0
  check "no OpenRouter key is reported as not signed in" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/models" | jq -r .authenticated.openrouter)" false
}

# Instructions in a project below the workspace are attached, to be followed, when a tool first
# touches it, whoever's repository it is; each is recorded once on the tape.
project_files() {
  local ws; ws=$(new_workspace project)
  local d; for d in cloned mine; do mkdir -p "$ws/$d"; echo "Always say PLANTED-$d." >"$ws/$d/AGENTS.md"; echo hi >"$ws/$d/README"; done
  git -C "$ws/cloned" init -q && git -C "$ws/cloned" remote add origin https://github.com/someone-else/cloned.git
  start_kernel "$ws" "$(script project-files.json)"
  local sid res; sid=$(zen ask --json -m faux/smoke "read cloned" | jq -r .session_id)
  res=$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='read'")
  check "a cloned repository's instructions are followed" bash -c 'grep -qF "Instructions for work under $2/cloned. Follow them." <<<"$1" && grep -q PLANTED-cloned <<<"$1"' _ "$res" "$ws"
  check "and recorded once" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='context'")" 1
  sid=$(zen ask --json -m faux/smoke "read mine" | jq -r .session_id)
  res=$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolName'='read'")
  check "so are the owner's own" bash -c 'grep -qF "Instructions for work under $2/mine. Follow them." <<<"$1" && grep -q PLANTED-mine <<<"$1"' _ "$res" "$ws"
}

# Secrets: the kernel's secret variables don't reach the agent's shell; a token in tool output is
# masked on the tape and in the recorded tool arguments (a long argument is cut, keeping its size
# and hash); a token in the URL (`?token=`) is refused for HTTP and the WebSocket: only the header counts.
secrets_masked() {
  local ws; ws=$(new_workspace secret)
  start_kernel "$ws" "$(script secret.json "s/BIGCONTENT/$(printf 'y%.0s' $(seq 1 5000))/")" E2E_PLANTED_KEY=planted-value-1234567890
  local sid; sid=$(zen ask --json -m faux/smoke "print the token" | jq -r .session_id)
  check "the kernel's secret variables are not in the agent's shell" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult' AND payload LIKE '%vars: 0%'")" 1
  local out; out=$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")
  check "the token is masked on the tape" grep -q 'ghp_…\[masked\]' <<<"$out"
  # (The command the model typed still contains it: masking applies to what tools return.)
  check "no tool output holds the token's value" eq "$(q "SELECT count(*) FROM tape_events WHERE payload->>'role'='toolResult' AND payload LIKE '%AbCdEfGhIjKlMnOp%'")" 0
  check "nor do the recorded tool arguments" eq "$(q "SELECT count(*) FROM tool_calls WHERE args LIKE '%AbCdEfGhIjKlMnOp%'")" 0
  check "where the token is masked too" eq "$(q "SELECT count(*) FROM tool_calls WHERE args->>'command' LIKE '%ghp_…[masked]%' AND session_id = X'${sid//-/}'")" 1
  check "a long argument is cut, with the whole one's size and hash" eq "$(q "SELECT length(args->>'content') < 4200 AND (args->'_full'->>'bytes') > 5000 AND length(args->'_full'->>'sha256') = 64 FROM tool_calls WHERE name = 'write' AND session_id = X'${sid//-/}'")" 1
  check "the file itself is whole" eq "$(wc -c <"$ws/big.txt")" 5000
  check "URL tokens are rejected for HTTP" eq "$(curl -s -o /dev/null -w '%{http_code}' "$URL/api/models?token=$TOKEN")" 401
  check "URL tokens are rejected for the WebSocket too" eq "$(curl -s -o /dev/null -w '%{http_code}' "$URL/api/sessions/$sid/ws?token=$TOKEN")" 401
}

# A turn the kernel ended (the watchdog, after an abort the worker ignored) keeps running in the
# worker and ends while the session's next turn runs: what it sends late must not reach that turn.
stale_turn() {
  local ws; ws=$(new_workspace stale)
  start_kernel "$ws" "$(script stale-turn.json)" ZEN_TURN_IDLE_SECS=1 ZEN_TURN_ABORT_GRACE_SECS=1
  local r sid; r=$(zen ask --json -m faux/smoke "first" || true); sid=$(echo "$r" | jq -r .session_id)
  check "the stuck turn was ended by the kernel" grep -q "stopped responding" <<<"$(echo "$r" | jq -r .error)"
  r=$(zen ask --json -s "$sid" "second" || true)
  check "the next turn ran to its end" eq "$(echo "$r" | jq -r .error)" null
  check "with its own answer" grep -q "second turn done" <<<"$(echo "$r" | jq -r .text)"
  check "the ended turn's late answer was dropped" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload LIKE '%LATE answer%'")" 0
}

# The kernel's boundary with its workers: a name in ZEN_WORKERS that isn't a known worker (a
# retired one) is ignored, not started, and doesn't fail the health check; workers get none of the
# kernel's secret variables, except the engines' sign-ins.
engine_boundary() {
  local ws; ws=$(new_workspace boundary)
  start_kernel "$ws" "" ZEN_WORKERS=engine,pi,envdump E2E_PLANTED_KEY=planted-worker-secret ANTHROPIC_API_KEY=e2e-sign-in \
    "ZEN_WORKER_ENVDUMP_CMD=env > $TMP/worker-env; exec python3 $REPO/scripts/e2e/slow_worker.py"
  local h; h=$(curl -s "$URL/health")
  check "an unknown worker name doesn't fail the health check" eq "$(echo "$h" | jq -c '[.ok, (.workers | keys)]')" '[true,["engine","envdump"]]'
  check "it is reported as ignored" eq "$(echo "$h" | jq -c .ignored_workers)" '["pi"]'
  check "and logged" grep -q "\`pi\`, which isn't a known worker" "$TMP/kernel.log"
  check "workers get no secret of the kernel's" bash -c '! grep -qE "^(E2E_PLANTED_KEY|ZEN_TOKEN|DATABASE_URL)=" "$1"' _ "$TMP/worker-env"
  check "but keep the engines' sign-ins" grep -qx "ANTHROPIC_API_KEY=e2e-sign-in" "$TMP/worker-env"
  check "a turn still runs" eq "$(zen ask --json -m faux/smoke "hi" | jq -r .error)" null
}

# A turn aborted while its tool runs: the tape still pairs every tool call with a result; a
# subagent the aborted turn waits for is stopped with it.
abort_tool() {
  local ws; ws=$(new_workspace aborttool)
  start_kernel "$ws" "$(script abort-tool.json)"
  local sid; sid=$(zen new --json -m faux/smoke | jq -r .id)
  python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$sid" '{"type":"prompt","text":"run the slow command"}' 1 3 >/dev/null
  check "the turn was interrupted" poll "SELECT outcome FROM turns WHERE session_id=X'${sid//-/}'" interrupted
  local calls="SELECT b.value->>'id' FROM tape_events, json_each(payload->'content') b WHERE session_id=X'${sid//-/}' AND payload->>'role'='assistant' AND b.value->>'type'='toolCall'"
  check "its tool call is on the tape" eq "$(q "SELECT count(*) FROM ($calls) c")" 1
  check "with a result saying it was interrupted" grep -q "interrupted" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'toolCallId' IN ($calls)")"
  check "and only one" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='toolResult'")" 1
  check "the next turn runs" eq "$(zen ask --json -s "$sid" "again" | jq -r .error)" null
  # Aborting a parent stops the subagent it is waiting for, not just the parent's own turn.
  local parent; parent=$(zen new --json -m faux/smoke | jq -r .id)
  python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$parent" '{"type":"prompt","text":"hand off a long nap"}' 1 3 >/dev/null
  check "the parent was interrupted" poll "SELECT outcome FROM turns WHERE session_id=X'${parent//-/}'" interrupted
  check "and so was its running subagent" poll "SELECT group_concat(t.outcome, ',') FROM turns t JOIN sessions s ON s.id = t.session_id WHERE s.parent=X'${parent//-/}'" interrupted
  check "whose command never finished" eq "$(q "SELECT count(*) FROM tape_events WHERE payload LIKE '%child-too-late%' AND payload->>'role'='toolResult'")" 0
}

# A session the old briefed workflow left mid-verification still works after an upgrade.
restart_recovery() {
  local ws; ws=$(new_workspace restart)
  start_kernel "$ws" ""
  local sid; sid=$(zen new --json -m faux/smoke | jq -r .id)
  q "UPDATE sessions SET state = 'verifying' WHERE id = X'${sid//-/}'" >/dev/null
  stop_kernel
  start_kernel "$ws" ""
  check "an old session in a workflow state takes a prompt" eq "$(zen ask --json -s "$sid" "hi" | jq -r .error)" null
}

# A summary made at the hard limit by a summarizer slower than the watchdog's idle limit (8 s: the
# 1-second limit plus the watchdog's 5-second tick): the turn waits for it instead of being taken
# for stalled. The `slow` worker is slow_worker.py
# (ZEN_WORKER_SLOW_CMD).
slow_summary() {
  local ws; ws=$(new_workspace slow)
  start_kernel "$ws" "$(script summary.json)" ZEN_CONTEXT_TOKENS=4000 ZEN_COMPACT_SOFT=100 ZEN_COMPACT_HARD=0.7 \
    ZEN_SUMMARY_MODEL=slow/summarizer ZEN_SLOW_SECS=8 ZEN_TURN_IDLE_SECS=1 ZEN_WORKERS=engine,slow \
    "ZEN_WORKER_SLOW_CMD=python3 $REPO/scripts/e2e/slow_worker.py"
  local sid; sid=$(zen ask --json -m faux/smoke "one" | jq -r .session_id)
  zen ask --json -s "$sid" "two" >/dev/null
  local r; r=$(zen ask --json -s "$sid" "three")
  check "the turn waited for the summary" eq "$(echo "$r" | jq -r .error)" null
  check "the summary was made by the slow summarizer and applied" eq "$(q "SELECT model FROM compactions WHERE session_id=X'${sid//-/}' AND applied_seq > 0")" slow/summarizer
  check "its tools ran (not interrupted)" eq "$(q "SELECT tool_errors || '/' || outcome FROM turns WHERE session_id=X'${sid//-/}' ORDER BY started_at DESC LIMIT 1")" 0/ok
}

# Scheduled jobs (jobs/): the kernel's own are seeded; an owner's job runs in a fresh session of
# kind `job` with the context it picked and can't schedule; its report is recorded, its session is
# archived however the run ends, failures pause the job; a run missed beyond its grace is skipped, one within it
# catches up once, a run cut by a restart is marked interrupted; a job the agent creates is live at
# once; a second run of a running job is refused; the system jobs are seeded, the `sleep` job runs
# through the same engine and each upkeep job (`wiki`, `skills`, `prune`) by hand once; `zen
# status` shows jobs. Uses `jobs.json`, the System One stub,
# ZEN_JOBS=1 and ZEN_JOBS_TICK=0.5 so runs start within a second.
scheduled_jobs() {
  local ws; ws=$(new_workspace jobs)
  local port=$((PORT + 3))
  python3 "$REPO/scripts/e2e/systemone_stub.py" "$port" & local srv=$!
  local s1=(ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key ZEN_S1_URL="http://127.0.0.1:$port/systemone")
  start_kernel "$ws" "$(script jobs.json)" ZEN_JOBS=1 ZEN_JOBS_TICK=0.5 ZEN_DEFAULT_MODEL=faux/smoke "${s1[@]}"
  local runs="SELECT group_concat(r.status || '/' || r.trigger, ',' ORDER BY r.id) FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name ="
  check "the system jobs are seeded" eq "$(zen jobs --json | jq -r '[.jobs[] | select(.kind == "system") | .name] | sort | join(",")')" engines,prune,skills,sleep,wiki
  zen jobs add report-a --schedule "every 1h" --prompt "JOBTASK-A" >/dev/null
  q "UPDATE jobs SET next_run_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE name = 'report-a'" >/dev/null
  check "a due job runs once" poll "$runs 'report-a'" ok/schedule
  check "its report is recorded" grep -q "Report A: all good." <<<"$(zen jobs runs report-a)"
  check "and its next run is an hour on" eq "$(q "SELECT next_run_at BETWEEN strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+55 minutes') AND strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+61 minutes') FROM jobs WHERE name = 'report-a'")" 1
  local sid; sid=$(q "SELECT session_id FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name = 'report-a'")
  check "in a session of kind job, archived once it ends" eq "$(zen sessions ls --archived --json | jq -r --arg s "$sid" '.[] | select(.id == $s) | .title')" "job: report-a"
  check "which can't schedule" grep -q "available in a scheduled job" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id = X'${sid//-/}' AND payload->>'toolName' = 'schedule'")"
  local base; base=$(q "SELECT payload->>'text' FROM tape_events WHERE session_id = X'${sid//-/}' AND kind = 'base'")
  check "with the job's context only: soul, owner, memory, the job note" bash -c 'grep -q "<soul" <<<"$1" && grep -q "<memory" <<<"$1" && grep -q "<scheduled_job name=\"report-a\"" <<<"$1" && ! grep -q "<identity" <<<"$1" && ! grep -q "<skills>" <<<"$1"' _ "$base"
  zen jobs add quiet --schedule "every 1h" --prompt "JOBTASK-S" >/dev/null
  zen jobs run quiet >/dev/null
  check "a silent run is recorded" poll "$runs 'quiet'" silent/owner
  check "and its session is out of sight" eq "$(q "SELECT s.archived FROM sessions s JOIN job_runs r ON r.session_id = s.id JOIN jobs j ON j.id = r.job_id WHERE j.name = 'quiet'")" 1
  zen jobs add broken --schedule "every 1h" --prompt "JOBTASK-F" >/dev/null
  q "UPDATE jobs SET failures = 4 WHERE name = 'broken'" >/dev/null
  zen jobs run broken >/dev/null
  check "a failed run is an error" poll "$runs 'broken'" error/owner
  check "and its session is archived too" eq "$(q "SELECT s.archived FROM sessions s JOIN job_runs r ON r.session_id = s.id JOIN jobs j ON j.id = r.job_id WHERE j.name = 'broken'")" 1
  check "the fifth failure in a row pauses the job" grep -q "paused after 5 failed runs in a row; last: the thing is broken" <<<"$(q "SELECT paused_reason FROM jobs WHERE name = 'broken' AND NOT enabled")"
  q "UPDATE jobs SET next_run_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-3 hours') WHERE name = 'report-a'" >/dev/null
  check "a run missed beyond its grace is skipped" poll "$runs 'report-a'" ok/schedule,missed/schedule
  q "UPDATE jobs SET next_run_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-10 minutes') WHERE name = 'report-a'" >/dev/null
  check "one within it catches up once" poll "$runs 'report-a'" ok/schedule,missed/schedule,ok/catchup
  zen jobs pause report-a >/dev/null
  q "INSERT INTO job_runs (job_id, trigger, status) SELECT id, 'schedule', 'running' FROM jobs WHERE name = 'quiet'" >/dev/null
  check "a second run of a running job is refused" grep -q "already running" <<<"$(zen jobs run quiet 2>&1)"
  stop_kernel
  start_kernel "$ws" "$(script jobs.json)" ZEN_JOBS=1 ZEN_JOBS_TICK=0.5 ZEN_DEFAULT_MODEL=faux/smoke "${s1[@]}"
  check "a run cut by a restart is marked interrupted" poll "$runs 'quiet'" silent/owner,interrupted/schedule
  local r; r=$(zen ask --json -m faux/smoke "please schedule a report")
  check "a job the agent creates is live at once" eq "$(q "SELECT enabled || '/' || created_by FROM jobs WHERE name = 'agent-made'")" 1/agent
  q "DELETE FROM sleep_runs" >/dev/null
  zen jobs run sleep >/dev/null
  check "a system job runs through the same engine" poll "$runs 'sleep'" ok/owner
  check "the sleep ran as the nightly one" eq "$(q "SELECT trigger FROM sleep_runs")" nightly
  local job before; before=$(q "SELECT coalesce(max(id), 0) FROM job_runs")
  for job in wiki skills prune; do zen jobs run "$job" >/dev/null; done
  check "each upkeep job runs by hand and records its run" poll \
    "SELECT group_concat(j.name || ':' || (r.status IN ('ok', 'silent')), ',' ORDER BY j.name) FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name IN ('wiki', 'skills', 'prune') AND r.id > $before AND r.status <> 'running'" \
    prune:1,skills:1,wiki:1
  check "zen status shows the jobs" grep -q "^jobs .*jobs (" <<<"$(zen status)"
  kill "$srv" 2>/dev/null || true
}

# Report files (jobs/report.rs): a job with `report_file` reports the file its run wrote, not the
# run's final answer; the path is stored absolute and must lie inside the job's workspace when set;
# a run that wrote no file, left an old one, or whose file is a symlink (to the token) or outside the
# workspace is an error with the reason; an empty file is a silent run; `--report-file ""` clears it.
job_report_file() {
  local ws; ws=$(new_workspace report-file)
  start_kernel "$ws" "$(script jobs.json)" ZEN_JOBS=1 ZEN_JOBS_TICK=0.5 ZEN_DEFAULT_MODEL=faux/smoke
  local runs="SELECT group_concat(r.status, ',' ORDER BY r.id) FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name ="
  local last="SELECT coalesce(r.error, r.output) FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE r.status <> 'running' AND j.name ="
  zen jobs add rfile --schedule "every 1h" --prompt "JOBTASK-R" --report-file out/message.md >/dev/null
  check "the report file is stored absolute" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "rfile") | .report_file')" "$ws/out/message.md"
  zen jobs run rfile >/dev/null
  check "a run that writes it is ok" poll "$runs 'rfile'" ok
  check "its report is the file, not the final answer" eq "$(q "$last 'rfile' ORDER BY r.id DESC LIMIT 1")" "REPORT FROM FILE: Palavra do Senhor."
  check "a path outside the workspace is refused when set" grep -q "inside the job's workspace" <<<"$(zen jobs add rout --schedule "every 1h" --prompt "JOBTASK-N" --report-file /etc/hostname 2>&1)"
  zen jobs add rmiss --schedule "every 1h" --prompt "JOBTASK-N" --report-file out/none.md >/dev/null
  zen jobs run rmiss >/dev/null
  check "a run that wrote no file is an error" poll "$runs 'rmiss'" error
  check "saying so, not posting the answer" grep -q "didn't write its report file" <<<"$(q "$last 'rmiss' ORDER BY r.id DESC LIMIT 1")"
  echo "yesterday's readings" >"$ws/out/stale.md" && touch -d '1 hour ago' "$ws/out/stale.md"
  zen jobs set rmiss --report-file out/stale.md >/dev/null
  zen jobs run rmiss >/dev/null
  check "a file older than the run is an error" poll "$runs 'rmiss'" error,error
  check "named as stale" grep -q "older than the run" <<<"$(q "$last 'rmiss' ORDER BY r.id DESC LIMIT 1")"
  ln -s "$TMP/home/.zenbot/token" "$ws/out/token.md"
  q "UPDATE jobs SET report_file = '$ws/out/token.md' WHERE name = 'rmiss'" >/dev/null
  zen jobs run rmiss >/dev/null
  check "a symlink to the token is an error" poll "$runs 'rmiss'" error,error,error
  check "named as a symlink" grep -q "is a symlink" <<<"$(q "$last 'rmiss' ORDER BY r.id DESC LIMIT 1")"
  q "UPDATE jobs SET report_file = '/etc/hostname' WHERE name = 'rmiss'" >/dev/null
  zen jobs run rmiss >/dev/null
  check "a file outside the workspace is an error at run time too" poll "$runs 'rmiss'" error,error,error,error
  check "named as outside" grep -q "inside the job's workspace" <<<"$(q "$last 'rmiss' ORDER BY r.id DESC LIMIT 1")"
  zen jobs add rempty --schedule "every 1h" --prompt "JOBTASK-E" --report-file out/empty.md >/dev/null
  zen jobs run rempty >/dev/null
  check "an empty file is a silent run" poll "$runs 'rempty'" silent
  zen jobs set rfile --report-file "" >/dev/null
  check "an empty --report-file clears it" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "rfile") | .report_file')" null
}

# Reports posted into a session (jobs/): a job with `post_to` (a session id or its prefix,
# stored whole; only the owner's own sessions) has an `ok` run's report appended to that session
# as one assistant message, recorded in `job_runs.posted_to` (so the Matrix bridge leaves it out of
# the jobs room); the session goes on afterwards; `--post-to ""` clears it.
job_post_to() {
  local ws; ws=$(new_workspace post-to)
  start_kernel "$ws" "$(script jobs.json)" ZEN_JOBS=1 ZEN_JOBS_TICK=0.5 ZEN_DEFAULT_MODEL=faux/smoke
  local runs="SELECT group_concat(r.status, ',' ORDER BY r.id) FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name ="
  local sid; sid=$(zen ask --json -m faux/smoke "OWNER-HELLO" | jq -r .session_id)
  local posted="SELECT count(*) FROM tape_events WHERE session_id = X'${sid//-/}' AND kind = 'message' AND payload->>'job' = 'ppost'"
  zen jobs add ppost --schedule "every 1h" --prompt "JOBTASK-A" --post-to "${sid:0:8}" >/dev/null
  check "the target is stored whole" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "ppost") | .post_to')" "$sid"
  zen jobs run ppost >/dev/null
  check "the run is ok" poll "$runs 'ppost'" ok
  check "and recorded as posted into the session" poll "SELECT posted_to FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name = 'ppost'" "$sid"
  check "its report is one assistant message there, attributed to the job" poll \
    "SELECT count(*) || ':' || min(payload->>'role' = 'assistant' AND payload->'content'->0->>'text' LIKE '%ppost%Report A: all good.%') FROM tape_events WHERE session_id = X'${sid//-/}' AND kind = 'message' AND payload->>'job' = 'ppost'" 1:1
  stop_kernel
  q "INSERT INTO job_runs (job_id, trigger, status, output, ended_at, pending_to)
     SELECT id, 'owner', 'ok', 'QUEUED BEFORE RESTART', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), X'${sid//-/}' FROM jobs WHERE name = 'ppost'" >/dev/null
  start_kernel "$ws" "$(script jobs.json)" ZEN_JOBS=1 ZEN_JOBS_TICK=0.5 ZEN_DEFAULT_MODEL=faux/smoke
  check "durable queued reports survive restart without reposting the first" poll "$posted" 2
  check "the session goes on after it" eq "$(zen ask --json -s "$sid" "OWNER-HELLO" | jq -r .text)" "Hello, owner."
  local jsid; jsid=$(q "SELECT session_id FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.name = 'ppost' AND r.session_id IS NOT NULL LIMIT 1")
  check "a job's own session is refused as a target" grep -q "no session of the owner's" <<<"$(zen jobs add pjob --schedule "every 1h" --prompt "JOBTASK-A" --post-to "$jsid" 2>&1)"
  check "an unknown one too" grep -q "no session of the owner's" <<<"$(zen jobs add pnone --schedule "every 1h" --prompt "JOBTASK-A" --post-to deadbeef 2>&1)"
  zen jobs set ppost --post-to "" >/dev/null
  check "an empty --post-to clears it" eq "$(zen jobs --json | jq -r '.jobs[] | select(.name == "ppost") | .post_to')" null
  # A report that can't be delivered (here a trigger refuses its target's tape) never holds up the
  # schedule: a due job still runs, and the report waits for its next try.
  local stuck; stuck=$(zen ask --json -m faux/smoke "OWNER-HELLO" | jq -r .session_id)
  q "CREATE TRIGGER e2e_refuse BEFORE INSERT ON tape_events
     WHEN NEW.session_id = X'${stuck//-/}' BEGIN SELECT RAISE(ABORT,'e2e: refused'); END;
     INSERT INTO job_runs (job_id, trigger, status, output, ended_at, pending_to) SELECT id, 'owner', 'ok', 'STUCK', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), X'${stuck//-/}' FROM jobs WHERE name = 'ppost'" >/dev/null
  zen jobs add pdue --schedule "every 1h" --prompt "JOBTASK-A" >/dev/null
  q "UPDATE jobs SET next_run_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE name = 'pdue'" >/dev/null
  check "a failing delivery doesn't stop a due job" poll "$runs 'pdue'" ok
  check "and that report stays queued for its next try" eq "$(q "SELECT count(*) FROM job_runs WHERE output = 'STUCK' AND pending_to = X'${stuck//-/}'")" 1
  q "DROP TRIGGER e2e_refuse" >/dev/null
}

# Session names and next-prompt suggestions (assist.rs): after each owner turn the assist model names
# the session (while the owner hasn't) and suggests the next prompt; the next prompt settles it.
names_suggestions() {
  local ws; ws=$(new_workspace names)
  start_kernel "$ws" "" ZEN_ASSIST_MODEL=faux/smoke
  local r sid; r=$(zen ask --json -m faux/smoke "hi"); sid=$(echo "$r" | jq -r .session_id)
  local s1="SELECT title || '/' || title_source FROM sessions WHERE id = X'${sid//-/}'"
  check "the session is named after the first turn" poll "$s1" "Scripted task/model"
  local open="SELECT count(*) FROM prompt_suggestions WHERE session_id = X'${sid//-/}' AND outcome IS NULL AND suggested = 'Run the tests again' AND prompt_version = 'v1'"
  check "a next prompt is suggested" poll "$open" 1
  local id ev; id=$(q "SELECT max(id) FROM prompt_suggestions WHERE session_id = X'${sid//-/}'")
  ev=$(python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$sid" "{\"type\":\"prompt\",\"text\":\"Run the tests again\",\"suggestion\":{\"id\":$id,\"taken\":true}}" 15)
  check "taken and sent unchanged: accepted" eq "$(q "SELECT outcome || '/' || coalesce(final, '-') FROM prompt_suggestions WHERE id = $id")" "accepted/-"
  check "the client is told the name and the new suggestion" bash -c 'grep -q "\"type\": \"title\"" <<<"$1" && grep -q "\"type\": \"suggestion\"" <<<"$1"' _ "$ev"
  id=$(echo "$ev" | jq -r 'select(.type == "suggestion") | .id')
  python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$sid" "{\"type\":\"prompt\",\"text\":\"Run the tests twice\",\"suggestion\":{\"id\":$id,\"taken\":true}}" 15 >/dev/null
  check "taken and changed: edited, with what was sent" eq "$(q "SELECT outcome || '/' || final FROM prompt_suggestions WHERE id = $id")" "edited/Run the tests twice"
  id=$(q "SELECT max(id) FROM prompt_suggestions WHERE session_id = X'${sid//-/}'")
  python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$sid" "{\"type\":\"prompt\",\"text\":\"something else\",\"suggestion\":{\"id\":$id,\"taken\":false}}" 15 >/dev/null
  check "the owner's own prompt: declined" eq "$(q "SELECT outcome || '/' || final FROM prompt_suggestions WHERE id = $id")" "declined/something else"
  check "a suggestion each turn" poll "SELECT count(*) FROM prompt_suggestions WHERE session_id = X'${sid//-/}'" 4
  id=$(q "SELECT max(id) FROM prompt_suggestions WHERE session_id = X'${sid//-/}'")
  zen ask --json -s "$sid" "one more" >/dev/null
  check "a client that doesn't show suggestions: unseen" eq "$(q "SELECT outcome FROM prompt_suggestions WHERE id = $id")" unseen
  # The owner's name is never overwritten.
  r=$(zen ask --json -m faux/smoke "hello"); sid=$(echo "$r" | jq -r .session_id)
  check "a second session is named" poll "SELECT title_source FROM sessions WHERE id = X'${sid//-/}'" model
  curl -s -X PATCH -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"title":"Mine"}' "$URL/api/sessions/$sid" >/dev/null
  zen ask --json -s "$sid" "again" >/dev/null
  check "a suggestion after the rename" poll "SELECT count(*) FROM prompt_suggestions WHERE session_id = X'${sid//-/}'" 2
  check "the owner's title stays" eq "$(q "SELECT title || '/' || title_source FROM sessions WHERE id = X'${sid//-/}'")" "Mine/owner"
  check "outcomes by prompt version" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/suggestions" | jq -c '.by_version[0] | [.prompt_version, .accepted, .edited, .declined, .unseen]')" '["v1",1,1,1,2]'
}

# A usage-limit failover moves the session to the model it failed over to, which it keeps until the
# owner picks another: the tape records the switch, the session and its clients show the new
# model, and the next turn runs on it.
failover_model() {
  local ws; ws=$(new_workspace failover)
  start_kernel "$ws" "$(script failover.json)"
  api() { curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
  local sid; sid=$(api -X POST -d '{"model":"faux/smoke"}' "$URL/api/sessions" | jq -r .id)
  local ev; ev=$(python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$sid" '{"type":"prompt","text":"first"}' 1)
  check "the failover is on the tape" eq "$(q "SELECT (payload->>'from') || '>' || (payload->>'to') FROM tape_events WHERE session_id=X'${sid//-/}' AND kind='failover'")" "faux>faux/backup"
  check "clients are told the session's new model" eq "$(echo "$ev" | jq -r 'select(.type == "model") | .model')" faux/backup
  check "the session keeps the model it failed over to" eq "$(api "$URL/api/sessions/$sid" | jq -r .model)" faux/backup
  check "the next turn runs on it" eq "$(zen ask --json -s "$sid" "second" | jq -r '.error // "ok"')/$(q "SELECT model FROM turns WHERE session_id=X'${sid//-/}' ORDER BY started_at DESC LIMIT 1")" "ok/faux/backup"
}

# Kernel behavior: queued and steered prompts, restart recovery, the tape mirror and a job run.
phase3_kernel() {
  local ws; ws=$(new_workspace phase3)
  local sp=$((PORT+12)) mp=$((PORT+13)) stub=$((PORT+15))
  python3 "$REPO/scripts/e2e/searxng_stub.py" "$sp" & local search_srv=$!
  python3 "$REPO/scripts/e2e/systemone_stub.py" "$mp" & local model_srv=$!
  mkdir -p "$TMP/www"; echo pong > "$TMP/www/ping"
  python3 -m http.server "$stub" --bind 127.0.0.1 --directory "$TMP/www" >/dev/null 2>&1 & local www_srv=$!
  sleep .3
  local faux; faux=$(script phase3.json "s|STUBPORT|$stub|g")
  start_kernel "$ws" "$faux" ZEN_DEFAULT_MODEL=faux/smoke ZEN_SEARXNG_URL="http://127.0.0.1:$sp" ZEN_S1_URL="http://127.0.0.1:$mp/systemone" ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key
  api() { curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
  local sid first second
  sid=$(api -X POST -d '{"model":"faux/smoke"}' "$URL/api/sessions" | jq -r .id)
  zen ask --json -s "$sid" P3-SLOW > "$TMP/first.json" & first=$!
  poll "SELECT count(*) FROM turns WHERE session_id=X'${sid//-/}' AND ended_at IS NULL" 1 >/dev/null
  python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$sid" '{"type":"prompt","text":"P3-QUEUED"}' 0 > "$TMP/queued.events" & second=$!
  wait "$first" "$second"
  check "a prompt sent while busy runs afterwards" poll "SELECT count(*) FROM tool_calls WHERE session_id=X'${sid//-/}' AND name='write'" 1
  check "the queued prompt is on the tape once" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='user' AND payload->>'content'='P3-QUEUED'")" 1
  check "the queue consumed it once" eq "$(q "SELECT count(*) FROM pending_prompts WHERE session_id=X'${sid//-/}'")/$(cat "$ws/queued.txt")" 0/queued
  zen ask --json -s "$sid" P3-BASE > "$TMP/base.json" & first=$!
  poll "SELECT count(*) FROM turns WHERE session_id=X'${sid//-/}' AND ended_at IS NULL" 1 >/dev/null
  python3 scripts/e2e/ws_prompt.py "$URL" "$TOKEN" "$sid" '{"type":"prompt","text":"P3-CORRECT","mode":"steer"}' 0 > "$TMP/steered.events" & second=$!
  wait "$first" "$second"
  check "owner steering stops the next action and runs the correction" poll "SELECT count(*) FROM tool_calls WHERE session_id=X'${sid//-/}' AND args->>'path'='steered.txt'" 1
  check "the action after the boundary did not run" test ! -e "$ws/should-not-run.txt"
  # Queue a prompt before stopping; the durable row must be dispatched after restart.
  stop_kernel
  q "INSERT INTO pending_prompts(session_id,text) VALUES(X'${sid//-/}','P3-QUEUED')" >/dev/null
  start_kernel "$ws" "$faux" ZEN_DEFAULT_MODEL=faux/smoke ZEN_SEARXNG_URL="http://127.0.0.1:$sp" ZEN_S1_URL="http://127.0.0.1:$mp/systemone" ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key
  check "a queued prompt survives kernel restart" poll "SELECT count(*) FROM tape_events WHERE session_id=X'${sid//-/}' AND payload->>'role'='user' AND payload->>'content'='P3-QUEUED'" 2
  check "JSONL mirror catches up and preserves sequence and hashes" until_ok python3 -c 'import json,sys; rows=[json.loads(x) for x in open(sys.argv[1])]; assert rows and all(r["seq"]==i+1 and r["hash"] for i,r in enumerate(rows)); assert sum(r["payload"].get("role")=="user" and r["payload"].get("content")=="P3-QUEUED" for r in rows)==2' "$TMP/home/.zenbot/sessions/$sid.jsonl"
  # A job's shell reaches a local host and writes its output file.
  zen jobs add p3-readings --schedule 'every 1h' --prompt "P3-INCIDENT fetch http://127.0.0.1:$stub/ping" --dir "$ws" >/dev/null
  zen jobs run p3-readings >/dev/null
  check "a job naming its source finishes and delivers" poll "SELECT last_status FROM jobs WHERE name='p3-readings'" ok
  check "and its output write runs" eq "$(cat "$ws/out/notes.json")" readings
  kill "$search_srv" "$model_srv" "$www_srv"; wait "$search_srv" "$model_srv" "$www_srv" 2>/dev/null || true
}

# Record files (people/projects, long-term memory, old knowledge records) through the owner's actions;
# the agent's remember (long-term past System One's high bar), refused direct writes, and search that
# follows typed links: kept links are followed, irrelevant ones dropped, a contradiction always shown,
# a superseded record replaced; meaning across English and Portuguese through vectors.
phase4_brain() {
  local ws; ws=$(new_workspace phase4)
  local s1port=$((PORT + 5)) embed_port=$((PORT + 7)) zh="$TMP/home/.zenbot"
  python3 "$REPO/scripts/e2e/systemone_stub.py" "$s1port" & local s1srv=$!
  local s1=(ZEN_DEFAULT_MODEL=faux/smoke ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key ZEN_S1_URL="http://127.0.0.1:$s1port/systemone")
  # Words only first (no vectors), so which records links bring in is deterministic.
  start_kernel "$ws" "$(script phase4.json "s|ZENGLOBAL|$zh/global|")" "${s1[@]}" ZEN_EMBED=0
  out() { q "SELECT group_concat(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id=X'$(jq -r .session_id <<<"$1" | tr -d -)' AND payload->>'role'='toolResult'"; }
  check "records through the owner's brain actions" python3 "$REPO/scripts/e2e/phase4.py" "$URL" "$TOKEN" "$TMP/home"
  local r; r=$(zen ask --json -m faux/smoke 'P4-REMEMBER')
  check "remember saves a kind" eq "$(q "SELECT record_type || '/' || tier FROM memories WHERE text LIKE 'We paused%'")" decision/short
  r=$(zen ask --json -m faux/smoke 'P4-FORGED')
  check "owner words that aren't the owner's are saved as an inference" bash -c '[ "$(jq -r ".tools[0].is_error" <<<"$1")" = false ] && [ "$2" = inferred ]' _ "$r" "$(q "SELECT source FROM memories WHERE text LIKE 'The owner always%'")"
  r=$(zen ask --json -m faux/smoke 'P4-LONG')
  local long; long=$(ls "$zh"/global/memory/*-long-we-keep-every-record*.md 2>/dev/null | head -1)
  check "a clearly lasting memory goes straight to long-term memory" bash -c 'test -s "$1" && grep -q "only indexes them" "$1"' _ "$long"
  check "and not to short-term memory" eq "$(q "SELECT count(*) FROM memories WHERE text LIKE 'LONG%'")" 0
  check "the decision is recorded" eq "$(q "SELECT substr(chosen,1,instr(chosen || ' ',' ')-1) FROM decisions WHERE point='memory_long' ORDER BY id DESC LIMIT 1")" long
  r=$(zen ask --json -m faux/smoke 'P4-MAYBE')
  check "below the high bar it is saved short-term" bash -c 'grep -q "not clearly lasting" <<<"$1" && [ "$2" = short ]' _ "$(out "$r")" "$(q "SELECT tier FROM memories WHERE text LIKE 'The demo might%'")"
  r=$(zen ask --json -m faux/smoke 'P4-DIRECT')
  check "the agent can't write memory files directly, nor put a new long-term memory" bash -c '[ "$(jq -r "[.tools[].is_error] | join(\",\")" <<<"$1")" = true,true ] && test ! -e "$2/global/memory/forged.md"' _ "$r" "$zh"
  r=$(zen ask --json -m faux/smoke 'P4-GET')
  check "brain get hands over a record with its links" grep -q '"to": "ana-partner"' <<<"$(out "$r")"
  # The graph: an irrelevant linked note, a contradicting claim, a superseded plan.
  # Knowledge records from before the wiki pages: files the index still reads (put refuses new ones).
  old_record() { mkdir -p "$zh/global/wiki/$1" && printf -- '---\n%s\n---\n\n%s\n' "$(jq -c 'del(.body)' <<<"$3")" "$(jq -r .body <<<"$3")" >"$zh/global/wiki/$1/$2.md"; }
  old_record concepts garden-notes '{"id":"garden-notes","store":"knowledge","kind":"concept","title":"Garden notes","body":"OFFTOPIC tomatoes and basil.","metadata":{"links":[{"relation":"mentions","target":"ana-partner"}]}}'
  old_record claims pilot-doubt '{"id":"pilot-doubt","store":"knowledge","kind":"claim","title":"A doubt","body":"OFFTOPIC the meeting notes may be wrong.","metadata":{"links":[{"relation":"contradicts","target":"pilot-meeting","evidence":[{"source":"meeting-source","locator":"line 3","basis":"interpretation"}]}]}}'
  old_record syntheses pilot-plan-v1 '{"id":"pilot-plan-v1","store":"knowledge","kind":"synthesis","title":"Pilot capacity plan, first draft","body":"Ana pilot capacity draft."}'
  old_record syntheses pilot-plan-v2 '{"id":"pilot-plan-v2","store":"knowledge","kind":"synthesis","title":"Revised plan","body":"The revised plan.","metadata":{"links":[{"relation":"supersedes","target":"pilot-plan-v1"}]}}'
  zen brain index >/dev/null
  r=$(zen ask --json -m faux/smoke 'P4-SEARCH'); local res; res=$(out "$r")
  check "search finds records and shows their links" bash -c 'grep -q "\`pilot-meeting\`" <<<"$1" && grep -q "links: .*about → ana-partner" <<<"$1"' _ "$res"
  check "it follows links System One keeps, and says how it got there" grep -q "reached by: pilot-meeting about ana-partner\|reached by: pilot-meeting about pilot-project" <<<"$res"
  check "an irrelevant linked record stays out" bash -c '! grep -q "garden-notes" <<<"$1"' _ "$res"
  check "a contradiction is always shown" grep -q "reached by: pilot-doubt contradicts pilot-meeting" <<<"$res"
  check "a superseded record is replaced by the newer one" bash -c 'grep -q "\`pilot-plan-v2\`" <<<"$1" && ! grep -q "\`pilot-plan-v1\`" <<<"$1"' _ "$res"
  old_record concepts fluxo '{"id":"fluxo","store":"knowledge","kind":"concept","title":"Blood circulation","body":"Blood circulation through the heart."}'
  zen brain index >/dev/null
  stop_kernel
  python3 "$REPO/scripts/e2e/embed_stub.py" "$embed_port" & local embed_server=$!
  start_kernel "$ws" "$(script phase4.json "s|ZENGLOBAL|$zh/global|")" "${s1[@]}" ZEN_EMBED_URL="http://127.0.0.1:$embed_port" ZEN_INDEX_SECS=1
  check "records are embedded for meaning" poll "SELECT count(*) FROM search_docs WHERE kind='record:knowledge' AND ref='fluxo' AND embedding IS NOT NULL" 1
  r=$(zen ask --json -m faux/smoke 'P4-LANG')
  check "a Portuguese query finds an English record through vectors" grep -q '`fluxo`' <<<"$(out "$r")"
  kill "$s1srv" "$embed_server"; wait "$s1srv" "$embed_server" 2>/dev/null || true
}

# Vault tasks (tasks.rs): `GET /api/tasks` reads the open `- [ ]` lines of each project's
# tasks.md under the zen home's global/Projects, grouped and sorted, behind the token; `zen tasks`
# lists them; the files are never written.
vault_tasks() {
  local ws; ws=$(new_workspace tasks)
  local p="$TMP/home/.zenbot/global/Projects"
  mkdir -p "$p/self" "$p/garden" "$p/work"
  printf '# self tasks\n\n- [ ] Plan the week 📅 2099-01-01 🆔 s1\n- [x] Old 🆔 s0\n- [-] Dropped 🆔 s9\n' >"$p/self/tasks.md"
  printf -- '- [ ] Later ⏬ 🆔 f2\n- [ ] Draft the seed order [[Garden first harvest]] ⏫️ 📅 2000-01-02 🆔 f1\n' >"$p/garden/tasks.md"
  printf '# work tasks\n' >"$p/work/tasks.md"
  local before; before=$(cat "$p"/*/tasks.md | sha256sum)
  # The agent a task is handed to runs on the default model: the faux one here.
  start_kernel "$ws" "" ZEN_DEFAULT_MODEL=faux/smoke
  check "tasks need the token" eq "$(curl -s -o /dev/null -w '%{http_code}' "$URL/api/tasks")" 401
  local t; t=$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/tasks")
  check "open tasks only, by the most pressing project" eq "$(jq -c '[.count, [.projects[] | .project], [.projects[].tasks[].id]]' <<<"$t")" '[3,["garden","self"],["f1","f2","s1"]]'
  check "a task's fields" eq "$(jq -c '.projects[0].tasks[0] | [.project, .text, .priority, .due, .overdue, .goals, .line, (.file | endswith("garden/tasks.md"))]' <<<"$t")" \
    '["garden","Draft the seed order","important","2000-01-02",true,["Garden first harvest"],2,true]'
  check "today in the owner's zone" eq "$(jq -r .tz <<<"$t")" UTC
  local out; out=$(zen tasks)
  check "zen tasks lists them by project" eq "$(head -3 <<<"$out")" "3 open tasks · $(jq -r .today <<<"$t")
**garden**
- ⏫ Draft the seed order · **overdue 2000-01-02** · → Garden first harvest \`f1\`"
  check "zen tasks --json is the kernel's answer" eq "$(zen tasks --json | jq .count)" 3
  # Handing a task to an agent (`a` in the Tasks view): a new session briefed with the task, linked
  # in the kernel by the task's id; asked again, the same agent comes back.
  local h; h=$(curl -s -X POST -H "Authorization: Bearer $TOKEN" "$URL/api/tasks/f1/agent")
  local sid; sid=$(jq -r .session <<<"$h")
  check "a task is handed to a new agent" eq "$(jq -c '[.task, .existing, .error]' <<<"$h")" '["f1",false,null]'
  check "its session is titled after the task" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid" | jq -r .title)" "task: Draft the seed order"
  check "its first prompt is the task's brief" bash -c 'grep -q "Draft the seed order" <<<"$1" && grep -q "Goal: \[\[Garden first harvest\]\]" <<<"$1" && grep -q "garden/tasks.md:2" <<<"$1"' _ \
    "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/sessions/$sid" | jq -r '[.messages[] | select(.role == "user")][0].content | if type == "string" then . else map(.text // "") | join("") end')"
  check "the task shows its agent" eq "$(curl -s -H "Authorization: Bearer $TOKEN" "$URL/api/tasks" | jq -r '.projects[0].tasks[0].agent.session')" "$sid"
  check "asked again, the same agent" eq "$(curl -s -X POST -H "Authorization: Bearer $TOKEN" "$URL/api/tasks/f1/agent" | jq -c '[.session == "'"$sid"'", .existing]')" '[true,true]'
  check "no open task, no agent" eq "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $TOKEN" "$URL/api/tasks/s0/agent")" 404
  check "the agent's turn ends" until_ok bash -c '[ "$(curl -s -H "Authorization: Bearer $1" "$2/api/tasks" | jq -r ".projects[0].tasks[0].agent.state")" != running ]' _ "$TOKEN" "$URL"
  check "zen tasks shows the agent" bash -c 'grep -q "\`f1\` · 🤖" <<<"$1"' _ "$(zen tasks)"
  check "the vault files are never written" eq "$(cat "$p"/*/tasks.md | sha256sum)" "$before"
}

retired_work_rejected() {
  local ws; ws=$(new_workspace retired-work)
  start_kernel "$ws" "$(script work.json)" ZEN_DEFAULT_MODEL=faux/smoke
  zen ask --json -m faux/smoke "try the retired work tool" >/dev/null
  check "retired work calls are refused and audited" eq "$(q "SELECT count(*) FROM tool_calls WHERE name='work' AND is_error")" 2
  check "no obsolete work records are created" eq "$(q "SELECT (SELECT count(*) FROM work_items)||'/'||(SELECT count(*) FROM work_attempts)")" 0/0
  check "no obsolete creation events are emitted" eq "$(q "SELECT count(*) FROM tape_events WHERE kind='work_created'")" 0
}

complete_api() {
  local ws; ws=$(new_workspace complete)
  start_kernel "$ws" ""
  api() { curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
  check "a completion needs the token" eq "$(curl -s -o /dev/null -w '%{http_code}' -X POST -d '{}' "$URL/api/complete")" 401
  local r; r=$(api -d '{"model": "faux/smoke", "system": "Say it.", "prompt": "FACT: alternatives work", "purpose": "e2e"}' "$URL/api/complete")
  check "one completion through the kernel, with the model that answered" eq "$(jq -r '[.model, (.text | contains("FACT: alternatives work"))] | join("|")' <<<"$r")" "faux/smoke|true"
  check "an empty prompt is refused" eq "$(api -o /dev/null -w '%{http_code}' -d '{"system": "x", "prompt": " "}' "$URL/api/complete")" 400
  check "an unknown model is a bad gateway, not a crash" eq "$(api -o /dev/null -w '%{http_code}' -d '{"model": "nope/none", "system": "x", "prompt": "y"}' "$URL/api/complete")" 502
  check "a search needs the token" eq "$(curl -s -o /dev/null -w '%{http_code}' -X POST -d '{}' "$URL/api/search")" 401
  q "INSERT INTO memories (text, source) VALUES ('the lighthouse keeper writes about the harbour', 'owner')" >/dev/null
  r=$(api -d '{"query": "lighthouse harbour", "scope": "memories"}' "$URL/api/search")
  check "the search tool through the API, as JSON results" eq "$(jq -r '[.scope, .results[0].kind, (.results[0].snippet | contains("lighthouse keeper"))] | join("|")' <<<"$r")" "memories|memory|true"
  check "a session's own scope is refused" eq "$(api -o /dev/null -w '%{http_code}' -d '{"query": "x", "scope": "this_session"}' "$URL/api/search")" 400
}

# Scenarios that share a setup run as parts of one; `scripts/e2e.sh <filter>` matches the scenario's
# name or a part's function name. Slowest first, so the parallel run ends sooner.
. "$REPO/scripts/e2e/plugins.sh"
. "$REPO/scripts/e2e/learning.sh"
run learning learning_loop
run phase4 phase4_brain
run phase3 phase3_kernel
run turns abort_tool stale_turn
run jobs scheduled_jobs job_report_file job_post_to script_jobs
run search-index search_index
run summaries summaries slow_summary
run system-one system_one_direct memory_sleep_judged
run file-trust kernel_tools project_files secrets_masked
run reach mcp_tools web_tools
run mcp-login mcp_login
run basics prompt_files open_loop restart_recovery layout_move vault_tasks complete_api
run memory memory_across_sessions memory_sleep
run retired-work retired_work_rejected
run delegation delegation engine_cap
run names-suggestions names_suggestions
run workshop workshop
run verifier verifier
run agent-tools skills_on_demand ask_and_gone_tools
run agents agents
run search search_recall
run wiki wiki_pages
run engine-boundary engine_boundary
run failover-model failover_model

# Run them, ZEN_E2E_JOBS at once, printing each one's output in order as it finishes.
STARTED=$SECONDS
shown=0 passed=0 times=()
show_done() { # print the finished scenarios in order, up to the first still running
  while [ "$shown" -lt "${#SCENARIOS[@]}" ] && [ -f "$ROOT/$shown.done" ]; do
    cat "$ROOT/$shown.out"
    local f secs name; read -r f secs name <"$ROOT/$shown.done"
    FAILED=$((FAILED + f)); [ "$f" = 0 ] && passed=$((passed + 1))
    times+=("$(printf '%5ss  %s' "$secs" "$name")")
    shown=$((shown + 1))
  done
}
for i in "${!SCENARIOS[@]}"; do
  while [ "$(jobs -rp | wc -l)" -ge "$JOBS" ]; do wait -n || true; show_done; done
  # shellcheck disable=SC2086
  ( in_scenario "$i" ${SCENARIOS[$i]} ) >"$ROOT/$i.out" 2>&1 &
done
wait || true
show_done
[ "$shown" = "${#SCENARIOS[@]}" ] || { echo "  FAIL  $(( ${#SCENARIOS[@]} - shown )) scenario(s) didn't finish"; FAILED=$((FAILED + 1)); }

echo
echo "seconds per scenario, slowest first:"; printf '%s\n' "${times[@]}" | sort -rn
echo "$passed of ${#SCENARIOS[@]} scenario(s) passed, $FAILED check(s) failed in $((SECONDS - STARTED))s ($JOBS at once)"
[ "$FAILED" = 0 ]
