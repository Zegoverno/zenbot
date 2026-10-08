#!/usr/bin/env bash
# End-to-end tests of the kernel with the scripted faux model: each scenario runs real turns through
# a kernel built from this checkout, on a throwaway database and workspace, and checks the result in
# the database. No subscription is used and the live service isn't touched.
#
#   scripts/e2e.sh              build, then run every scenario
#   scripts/e2e.sh memory       only scenarios whose name contains "memory"
#
# Needs Docker with Postgres from deploy/compose.yaml, git, curl, jq and bubblewrap.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.cargo/bin:$PATH"
. "$REPO/scripts/db.sh"
FILTER=${1:-}
PORT=${ZEN_E2E_PORT:-18377}
TOKEN=e2e-token-$$
DB=zen_e2e_$$
TMP=$(mktemp -d)
BIN="$REPO/target/release"
URL="http://127.0.0.1:$PORT"
KERNEL_PID=""
FAILED=0
PASSED=0

DB_URL=$(db_url_for "$DB")
q() { db_psql -d "$DB" -c "$1"; }
stop_kernel() { if [ -n "$KERNEL_PID" ]; then kill "$KERNEL_PID" 2>/dev/null || true; wait "$KERNEL_PID" 2>/dev/null || true; fi; KERNEL_PID=""; }
cleanup() { stop_kernel; [ -n "${ZEN_E2E_KEEP:-}" ] && { echo "kept: database $DB, files $TMP"; return; }; db_drop "$DB" || true; rm -rf "$TMP"; }
trap cleanup EXIT

[ -n "${ZEN_E2E_NO_BUILD:-}" ] || cargo build --release -q
db_psql -d postgres -c "CREATE DATABASE $DB" >/dev/null

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
    export ZEN_TOKEN=$TOKEN ZEN_PORT=$PORT ZEN_WORKERS=engine ZEN_FAUX=1 ZEN_FAUX_SCRIPT=$script ZEN_WORKSPACE=$ws \
      ZEN_ENGINE_CMD="$BIN/zen-engine" DATABASE_URL="$DB_URL" HOME="$TMP/home"
    unset ZEN_S1_MODEL OPENROUTER_API_KEY
    for kv in "$@"; do export "$kv"; done
    mkdir -p "$HOME"
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
eq() { [ "$1" = "$2" ] || { echo "        expected [$2], got [$1]" >&2; return 1; }; }

# Every session's tape is numbered 1..n without gaps, and its hash chain recomputes.
tape_is_sound() {
  eq "$(q "SELECT count(*) FROM (SELECT session_id, bool_and(seq IS NOT NULL) AND max(seq) = count(*) AS ok FROM tape_events GROUP BY session_id) s WHERE NOT ok")" 0 &&
  eq "$(q "WITH RECURSIVE c AS (
             SELECT session_id, seq, hash, zen_block_hash(NULL, kind, payload) AS h FROM tape_events WHERE seq = 1
             UNION ALL
             SELECT t.session_id, t.seq, t.hash, zen_block_hash(c.h, t.kind, t.payload)
             FROM tape_events t JOIN c ON t.session_id = c.session_id AND t.seq = c.seq + 1)
           SELECT count(*) FROM c WHERE hash <> h")" 0
}

run() { # run <name> <function>
  [ -z "$FILTER" ] || [[ $1 == *"$FILTER"* ]] || return 0
  echo "== $1"
  local before=$FAILED
  "$2" || { echo "  FAIL  scenario errored"; FAILED=$((FAILED + 1)); }
  stop_kernel
  if [ "$FAILED" = "$before" ]; then PASSED=$((PASSED + 1)); else echo "  (kernel log: $TMP/kernel.log)"; cp "$TMP/kernel.log" "$TMP/kernel-$1.log" 2>/dev/null || true; fi
}

# ---------- scenarios ----------

open_loop() {
  local ws; ws=$(new_workspace open)
  start_kernel "$ws" ""
  local r; r=$(zen ask --json -m faux/smoke "run the smoke command")
  check "answer" eq "$(echo "$r" | jq -r .error)" null
  check "the bash tool ran" eq "$(echo "$r" | jq -r '[.tools[].name] | join(",")')" bash
}

# The prompt files: installed when missing, never overwritten, read at session start.
prompt_files() {
  local ws; ws=$(new_workspace prompt)
  mkdir -p "$TMP/home/.zenbot" && echo "# USER.md
The owner's name is E2E Owner." >"$TMP/home/.zenbot/USER.md"
  start_kernel "$ws" ""
  local zh="$TMP/home/.zenbot"
  check "defaults installed, scoped" test -s "$zh/agents/zenbot/SOUL.md" -a -s "$zh/AGENTS.md" -a -s "$zh/global/skills/work/verify/SKILL.md" -a -s "$zh/global/skills/work/brief/references/template.md"
  check "a fresh home has no old-layout paths" test ! -e "$zh/SOUL.md" -a ! -e "$zh/skills"
  check "the owner's USER.md was kept" grep -q "E2E Owner" "$zh/USER.md"
  local sid; sid=$(zen ask --json -m faux/smoke "hi" | jq -r .session_id)
  local base; base=$(q "SELECT payload->>'text' FROM tape_events WHERE session_id='$sid' AND kind='base'")
  check "SOUL.md is in the instructions, from the agent's folder" grep -qF '<soul file="~/.zenbot/agents/zenbot/SOUL.md">' <<<"$base"
  check "AGENTS.md is the environment, placeholders filled" grep -qF "Working directory for tools: $ws" <<<"$base"
  check "USER.md is in the instructions" grep -q "E2E Owner" <<<"$base"
  check "the skills index is in the instructions" grep -q "work/verify:" <<<"$base"
  check "memory starts empty" grep -q "(empty)" <<<"$base"
  local tools; tools=$(q "SELECT string_agg(t->>'name', ',') FROM turns, envelopes e, jsonb_array_elements(e.tools) t WHERE turns.session_id='$sid' AND e.hash = turns.envelope")
  check "the system tools, in order, no workflow tools" eq "$tools" bash,read,write,edit,history,search,ask,remember,capture,web_search,web_fetch,find_skills,load_skill,save_skill,find_tools,load_tool,call_tool,save_tool,verify,delegate
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
  check "the moved soul is in the instructions" grep -q "E2E soul, moved" <<<"$(q "SELECT payload->>'text' FROM tape_events WHERE session_id='$sid' AND kind='base'")"
  stop_kernel
  start_kernel "$ws" "" ZEN_HOME="$zh"
  check "a second start changes nothing" eq "$(grep -c 'moved ' "$TMP/kernel.log") $(readlink "$zh/SOUL.md") $(cat "$zh/agents/zenbot/SOUL.md" | tail -1)" \
    "0 agents/zenbot/SOUL.md I am the E2E soul, moved."
}

# Skills load on demand, as tool results; nothing outside a skill's folder can be read through them.
skills_on_demand() {
  local ws; ws=$(new_workspace skills)
  start_kernel "$ws" "$(script agent.json)"
  local r sid; r=$(zen ask --json -m faux/smoke "use a skill"); sid=$(echo "$r" | jq -r .session_id)
  check "find, load, load a file, refused outside" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" find_skills:false,load_skill:false,load_skill:false,load_skill:true
  local res; res=$(q "SELECT string_agg(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "find_skills names it" grep -q "work/verify:" <<<"$res"
  check "load_skill returns SKILL.md" grep -q "Decide whether a fresh verifier adds something" <<<"$res"
  check "and a reference file" grep -q "## Criteria" <<<"$res"
  check "the instructions didn't change mid-session" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND kind='envelope'")" 1
}

# MCP: tools from a local (stdio) and a remote (HTTP) server, found, loaded and called through the
# three fixed tools; a call missing a required argument is refused by the kernel.
mcp_tools() {
  local ws; ws=$(new_workspace mcp)
  local port=$((PORT + 1))
  python3 "$REPO/scripts/e2e/mcp_server.py" --http "$port" & local srv=$!
  mkdir -p "$TMP/home/.zenbot"
  cat >"$TMP/home/.zenbot/mcp.json" <<JSON
{ "mcpServers": {
  "local": { "command": "python3", "args": ["$REPO/scripts/e2e/mcp_server.py"] },
  "remote": { "url": "http://127.0.0.1:$port/mcp", "headers": { "Authorization": "Bearer \${ZEN_E2E_MCP_TOKEN}" } } } }
JSON
  start_kernel "$ws" "$(script reach.json)" ZEN_E2E_MCP_TOKEN=e2e
  local r sid; r=$(zen ask --json -m faux/smoke "mcp please"); sid=$(echo "$r" | jq -r .session_id)
  check "find, load, call local, call remote, refused without an argument" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" \
    find_tools:false,load_tool:false,call_tool:false,call_tool:false,call_tool:true
  local res; res=$(q "SELECT string_agg(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "find_tools lists namespaced tools" grep -q "local_echo: Echo a message back." <<<"$res"
  check "find_tools wraps untrusted descriptions" grep -q 'source="mcp" about="remote_add"' <<<"$res"
  check "load_tool shows the schema" grep -q '"required"' <<<"$res"
  check "the local server answered" grep -q "echo: hi" <<<"$res"
  check "the remote server answered (event stream), wrapped as untrusted" grep -q 'source="mcp" about="remote_add"' <<<"$res"
  check "a missing argument is named" grep -q "missing required arguments: b" <<<"$res"
  check "the remote call tainted the session" eq "$(q "SELECT tainted_at IS NOT NULL FROM sessions WHERE id='$sid'")" t
  check "the tool list is still fixed" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND kind='envelope'")" 1
  kill "$srv" 2>/dev/null || true
}

# Web: private addresses are refused; search results come back numbered and wrapped as untrusted,
# with markers inside them defused; what a tainted session saves to memory counts as inference.
web_tools() {
  local ws; ws=$(new_workspace web)
  local port=$((PORT + 2))
  python3 "$REPO/scripts/e2e/searxng_stub.py" "$port" & local srv=$!
  start_kernel "$ws" "$(script reach.json)" ZEN_SEARXNG_URL="http://127.0.0.1:$port"
  local r sid; r=$(zen ask --json -m faux/smoke "web please"); sid=$(echo "$r" | jq -r .session_id)
  check "localhost and metadata refused, search ran, remember ran" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" \
    web_fetch:true,web_fetch:true,web_search:false,remember:false
  local res; res=$(q "SELECT string_agg(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "loopback refused" grep -q "127.0.0.1 is not a public address" <<<"$res"
  check "metadata refused" grep -q "169.254.169.254 is not a public address" <<<"$res"
  check "results numbered, non-http dropped" bash -c 'grep -q "\[2\] Another" <<<"$1" && ! grep -q "javascript:" <<<"$1"' _ "$res"
  check "one envelope, its marker defused" eq "$(grep -o '</untrusted>' <<<"$res" | wc -l)" 1
  check "the session is tainted" eq "$(q "SELECT tainted_at IS NOT NULL FROM sessions WHERE id='$sid'")" t
  check "its memory counts as inference" eq "$(q "SELECT source FROM memories WHERE text LIKE 'Something read on the web.%'")" inferred
  q "DELETE FROM memories; ALTER SEQUENCE memories_id_seq RESTART" >/dev/null  # the memory scenarios start from none
  kill "$srv" 2>/dev/null || true
}

# Search: a fact from one session is found from another (by words, and by its exact path first), a
# memory is found and counted as used, history reads the other session, and a promotion the owner
# accepts makes the memory long-term and still findable.
search_recall() {
  local ws; ws=$(new_workspace search)
  q "DELETE FROM memories; ALTER SEQUENCE memories_id_seq RESTART" >/dev/null
  start_kernel "$ws" "$(script search.json)"
  local a; a=$(zen ask --json -m faux/smoke "save: the purple elephant config lives in deploy/elephant.yaml" | jq -r .session_id)
  sleep 6  # the indexer takes events once they're 5 seconds old (search.rs)
  local s="$TMP/search-filled.json"; sed "s/SESSION/${a:0:8}/" "$REPO/scripts/e2e/search.json" >"$s"
  stop_kernel; start_kernel "$ws" "$s"
  local r sid; r=$(zen ask --json -m faux/smoke "find it"); sid=$(echo "$r" | jq -r .session_id)
  check "searches and history ran" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" search:false,search:false,search:false,history:false
  local res; res=$(q "SELECT string_agg(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "found by words, in the other session" grep -q "session ${a:0:8}" <<<"$res"
  check "the exact path comes first" grep -q "(exact match)" <<<"$res"
  check "the memory is found" grep -q "m1 — short-term memory (owner)" <<<"$res"
  check "and counted as used" eq "$(q "SELECT uses FROM memories WHERE id = 1")" 1
  check "history read the other session" grep -q "purple elephant" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='history'")"
  check "every search is logged" eq "$(q "SELECT count(*) FROM searches WHERE session_id='$sid'")" 3
  # The sleep proposed m1 for long-term memory; the owner accepts it.
  q "UPDATE memories SET tier='archived', proposed='promote' WHERE id=1; INSERT INTO decisions (point, input, chosen, acted) VALUES ('sleep', '{\"memory\": 1}', 'promote', false)" >/dev/null
  check "a non-proposed memory can't be accepted" bash -c '! ZEN_URL=$1 ZEN_TOKEN=$2 timeout 30 "$3/zen" memory accept m99 >/dev/null 2>&1' _ "$URL" "$TOKEN" "$BIN"
  zen memory accept m1 >/dev/null
  check "accepted: long-term" eq "$(q "SELECT tier || '/' || coalesce(proposed, '-') FROM memories WHERE id=1")" long/-
  check "the review is recorded for calibration" eq "$(q "SELECT actual || '/' || actual_by FROM decisions WHERE point='sleep' AND chosen='promote'")" accept/owner
  local l; l=$(zen ask --json -m faux/smoke "long term?" | jq -r .session_id)
  check "a long-term memory is found by search" grep -q "m1 — long-term memory" <<<"$(q "SELECT string_agg(payload->'content'->0->>'text', '|') FROM tape_events WHERE session_id='$l' AND payload->>'role'='toolResult'")"
  q "DELETE FROM memories; ALTER SEQUENCE memories_id_seq RESTART; DELETE FROM search_docs WHERE kind='memory'" >/dev/null
}

# Wiki: capture makes a page and appends to it (same title), commits, keeps the index and the log,
# masks secrets; search finds the page; a capture after reading the web is labelled web; the sleep
# reports a page that still has no summary.
wiki_capture() {
  local ws; ws=$(new_workspace wiki)
  local port=$((PORT + 2))
  python3 "$REPO/scripts/e2e/searxng_stub.py" "$port" & local srv=$!
  start_kernel "$ws" "$(script wiki.json)" ZEN_SEARXNG_URL="http://127.0.0.1:$port"
  local w="$TMP/home/.zenbot/global/wiki"
  local r; r=$(zen ask --json -m faux/smoke "first")
  check "a new page" bash -c 'grep -q "Captured to \[\[dom-smoothie\]\] (new page" <<<"$1"' _ "$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE payload->>'toolName'='capture' ORDER BY id DESC LIMIT 1")"
  check "it has a dated entry with its source" grep -q "(verified) — dom_smoothie ports readability.js" "$w/dom-smoothie.md"
  sleep 6
  r=$(zen ask --json -m faux/smoke "second"); local sid; sid=$(echo "$r" | jq -r .session_id)
  check "capture, then search" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" capture:false,search:false
  check "the same page, two entries, newest first" eq "$(grep -c '^- \*\*' "$w/dom-smoothie.md"):$(grep -m1 '^- \*\*' "$w/dom-smoothie.md" | grep -c 'Markdown text mode')" 2:1
  check "the secret is masked" bash -c '! grep -q "0123456789abcdef0123" "$1"' _ "$w/dom-smoothie.md"
  check "committed in git" bash -c '[ "$(git -C "$1" log --oneline | wc -l)" -ge 2 ]' _ "$w"
  check "listed in index.md, logged" bash -c 'grep -q "\[\[dom-smoothie\]\]" "$1/index.md" && grep -q "capture | dom-smoothie" "$1/log.md"' _ "$w"
  check "search finds the page" grep -q "wiki \[\[dom-smoothie\]\]" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='search'")"
  zen ask --json -m faux/smoke "web" >/dev/null
  check "a capture after reading the web is labelled web" grep -q "(web) — SearXNG answers JSON" "$w/searxng.md"
  local note; note=$(zen memory sleep --json | jq -r .note)
  check "the sleep reports pages without a summary" grep -q "wiki: dom-smoothie: no summary yet" <<<"$note"
  kill "$srv" 2>/dev/null || true
}

# Workshop: a new skill is a draft, a near-duplicate is refused, a reason is required, a new domain
# waits for the owner; drafts can be found and loaded; accepting a session that loaded a draft
# activates it; a made tool runs sandboxed (files read-only) until the owner approves it.
workshop() {
  local ws; ws=$(new_workspace workshop)
  start_kernel "$ws" "$(script workshop.json)"
  local sk="$TMP/home/.zenbot/global/skills"
  local r; r=$(zen ask --json -m faux/smoke "make")
  check "draft saved, duplicate refused, reason required, new domain, tool saved" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" \
    save_skill:false,save_skill:true,save_skill:true,save_skill:false,save_tool:false
  local sid; sid=$(echo "$r" | jq -r .session_id)
  local res; res=$(q "SELECT string_agg(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "the duplicate is told to extend the existing skill" grep -q "work/release-notes already does this job: extend it instead" <<<"$res"
  check "a new domain needs the owner" grep -q "finance\` is a new domain" <<<"$res"
  check "drafts live under _proposed" test -s "$sk/_proposed/work/release-notes/SKILL.md" -a -s "$sk/_proposed/finance/budget-review/SKILL.md"
  r=$(zen ask --json -m faux/smoke "use"); sid=$(echo "$r" | jq -r .session_id)
  check "find, load a draft, find and run a made tool" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" find_skills:false,load_skill:false,find_tools:false,call_tool:false
  res=$(q "SELECT string_agg(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "the draft is marked" grep -q "work/release-notes: (draft)" <<<"$res"
  check "the made tool is found" grep -q "made_word-count: Count the words" <<<"$res"
  check "it ran, sandboxed: files read-only" bash -c 'grep -q "^3" <<<"$1" && grep -q "write: refused" <<<"$1" && grep -q "ran sandboxed" <<<"$1"' _ "$res"
  zen sessions decide "$sid" accept >/dev/null; sleep 2
  check "an accepted session activates the draft it used" test -s "$sk/work/release-notes/SKILL.md"
  check "a new domain stays a draft" test -s "$sk/_proposed/finance/budget-review/SKILL.md"
  zen skills accept finance/budget-review >/dev/null
  check "the owner activates the new domain's skill" test -s "$sk/finance/budget-review/SKILL.md"
  zen tools accept word-count >/dev/null
  r=$(zen ask --json -m faux/smoke "again"); sid=$(echo "$r" | jq -r .session_id)
  check "approved: it can write" grep -q "write: ok" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='call_tool'")"
  echo "# changed after approval" >>"$TMP/home/.zenbot/global/tools/word-count/run.py"
  r=$(zen ask --json -m faux/smoke "again"); sid=$(echo "$r" | jq -r .session_id)
  check "changed after approval: sandboxed again" grep -q "changed since the owner approved it" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='call_tool'")"
  check "skills and tools are in git" bash -c '[ "$(git -C "$1" log --oneline | wc -l)" -ge 4 ] && [ "$(git -C "$2" log --oneline | wc -l)" -ge 1 ]' _ "$sk" "$TMP/home/.zenbot/global/tools"
  check "zen skills lists use" grep -q "work/release-notes" <<<"$(zen skills)"
}

# Delegation: subagents run their own turns (one on a named model, one routed by the owner's policy),
# can't ask or delegate, report back; each choice is logged with its probability, and the owner's
# verdict on the parent is the evidence `zen policy` shows. ask with wait=false keeps the turn going.
delegation() {
  local ws; ws=$(new_workspace delegate)
  start_kernel "$ws" "$(script delegate.json)"
  zen policy set default faux/smoke >/dev/null
  local r sid; r=$(zen ask --json -m faux/smoke "split the work"); sid=$(echo "$r" | jq -r .session_id)
  check "two delegations" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" delegate:false,delegate:false
  local res; res=$(q "SELECT string_agg(payload->'content'->0->>'text', '|' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='delegate'")
  check "named model, then the policy's" bash -c 'grep -q "on faux/smoke (asked, asked)" <<<"$1" && grep -q "(unknown, policy)" <<<"$1"' _ "$res"
  check "the subagents' answers come back" eq "$(grep -c '^Done\.$' <<<"${res//|/$'\n'}")" 2
  check "the subagents ran their task" eq "$(q "SELECT count(*) FROM sessions WHERE parent='$sid' AND kind='subagent'")" 2
  local child; child=$(q "SELECT id FROM sessions WHERE parent='$sid' AND kind='subagent' LIMIT 1")
  local tools; tools=$(q "SELECT string_agg(t->>'name', ',') FROM turns, envelopes e, jsonb_array_elements(e.tools) t WHERE turns.session_id='$child' AND e.hash = turns.envelope")
  check "a subagent can't ask or delegate" bash -c '! grep -qE "(^|,)(ask|delegate)(,|$)" <<<"$1" && grep -q "save_skill" <<<"$1"' _ "$tools"
  check "choices logged with their probability" eq "$(q "SELECT string_agg(chosen || '@' || probability, ',' ORDER BY id) FROM decisions WHERE point='model'")" faux/smoke@1,faux/smoke@1
  zen sessions decide "$sid" accept >/dev/null
  check "the owner's verdict is the evidence" grep -q '"accepted": 2' <<<"$(zen policy --json | jq '{accepted: ([.stats[].accepted] | add)}')"
  zen policy undo >/dev/null
  check "undo restores the earlier policy" eq "$(zen policy --json | jq -c .policy)" '{"routes":{}}'
  r=$(zen ask --json -m faux/smoke "waitless")
  check "ask with wait=false keeps working" eq "$(echo "$r" | jq -r '[.tools[] | "\(.name):\(.is_error)"] | join(",")')" ask:false,bash:false
}

# remember: a memory saved in one session is in the next session's instructions, not the current one's.
memory_across_sessions() {
  local ws; ws=$(new_workspace memory)
  start_kernel "$ws" "$(script agent.json)"
  local s1; s1=$(zen ask --json -m faux/smoke "remember this" | jq -r .session_id)
  check "saved with its source" eq "$(q "SELECT source || '/' || tier FROM memories")" owner/short
  check "exported to MEMORY.md" grep -q "\[m1\] The owner prefers tabs" "$TMP/home/.zenbot/global/MEMORY.md"
  zen ask --json -s "$s1" "and now" >/dev/null
  check "frozen for the session that wrote it" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$s1' AND kind='base' AND payload->>'text' LIKE '%prefers tabs%'")" 0
  local s2; s2=$(zen ask --json -m faux/smoke "hello" | jq -r .session_id)
  check "in the next session's instructions" grep -q "\[m1\] The owner prefers tabs" <<<"$(q "SELECT payload->>'text' FROM tape_events WHERE session_id='$s2' AND kind='base'")"
}

# The sleep tidies short-term memory to its size, archives (never deletes) and records what it did.
memory_sleep() {
  local ws; ws=$(new_workspace sleep)
  start_kernel "$ws" "$(script agent.json)" ZEN_MEMORY_CHARS=1000
  q "DELETE FROM decisions WHERE point='sleep'; DELETE FROM sleep_runs; DELETE FROM memories" >/dev/null
  q "INSERT INTO memories (text, source, updated_at) SELECT 'memory number ' || i || ' ' || repeat('x', 80), 'inferred', now() - (i || ' hours')::interval FROM generate_series(1, 30) i" >/dev/null
  local r; r=$(zen ask --json -m faux/smoke "remember this")
  check "over the hard limit, add is refused" eq "$(echo "$r" | jq -r '.tools[0].is_error')" true
  for _ in $(seq 1 20); do [ "$(q "SELECT count(*) FROM sleep_runs WHERE ended_at IS NOT NULL")" -gt 0 ] && break; sleep 0.5; done
  check "and starts a sleep" eq "$(q "SELECT trigger FROM sleep_runs ORDER BY id LIMIT 1")" ceiling
  r=$(zen memory sleep --json)
  check "a sleep by hand reports what it did" eq "$(echo "$r" | jq -r .entries)" "$(echo "$r" | jq -r '.kept + .dropped')"
  check "memory fits its size" test "$(q "SELECT COALESCE(SUM(length(text) + 10), 0) FROM memories WHERE tier='short'")" -le 1000
  check "the freshest were kept" eq "$(q "SELECT count(*) FROM memories WHERE tier='short' AND text LIKE 'memory number 1 %'")" 1
  check "nothing deleted" eq "$(q "SELECT count(*) FROM memories")" 30
  check "every entry's fate is a decision" test "$(q "SELECT count(*) FROM decisions WHERE point='sleep'")" -ge 30
  local s2; s2=$(zen ask --json -m faux/smoke "hello" | jq -r .session_id)
  check "the next session hears about the sleep" grep -q "Last sleep" <<<"$(q "SELECT payload->>'text' FROM tape_events WHERE session_id='$s2' AND kind='base'")"
  check "zen memory lists it" grep -q "last sleep" <<<"$(zen memory)"
}

# ask ends the turn; tools that are gone are refused.
ask_and_gone_tools() {
  local ws; ws=$(new_workspace ask)
  start_kernel "$ws" "$(script agent.json)"
  local r sid; r=$(zen ask --json -m faux/smoke "ask me"); sid=$(echo "$r" | jq -r .session_id)
  check "the questions were recorded" eq "$(q "SELECT payload->'questions'->0->>'question' FROM tape_events WHERE session_id='$sid' AND kind='questions'")" "Which flag name?"
  check "nothing runs after ask in that turn" eq "$(echo "$r" | jq -r '[.tools[].is_error] | join(",")')" false,true
  r=$(zen ask --json -m faux/smoke "move it")
  check "move is gone" eq "$(echo "$r" | jq -r '.tools[0].is_error')" true
  check "README not moved" test -e "$ws/README"
}

# verify: the kernel runs the commands, a fresh read-only verifier judges the rest.
verifier() {
  local ws; ws=$(new_workspace verifier)
  start_kernel "$ws" "$(script judgment.json 's/TARGET/done.txt/')"
  local r sid; r=$(zen ask --json -m faux/smoke "Please create done.txt"); sid=$(echo "$r" | jq -r .session_id)
  local child; child=$(q "SELECT id FROM sessions WHERE parent='$sid' AND kind='verifier'")
  check "a criterion needing judgment ran the verifier" test -n "$child"
  check "the verifier works in the repository" grep -q "^$ws" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$child' AND payload->>'toolName'='bash' ORDER BY seq LIMIT 1")"
  check "the verifier can't write" test ! -e "$ws/verifier-wrote.txt" -a ! -e "$ws/verifier-touched.txt"
  check "command passed, judgment uncertain" eq "$(q "SELECT string_agg(x->>'result', ',') FROM tape_events, jsonb_array_elements(payload->'results') x WHERE session_id='$sid' AND kind='verification'")" pass,uncertain
  check "the result went back to the model" grep -q "1 passed, 0 failed, 1 uncertain" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='verify'")"
  # A failed command needs no verifier: its output is the evidence.
  stop_kernel
  start_kernel "$ws" "$(script judgment.json 's/TARGET/missing.txt/')"
  sid=$(zen ask --json -m faux/smoke "again" | jq -r .session_id)
  check "a failed command skips the verifier" eq "$(q "SELECT count(*) FROM sessions WHERE parent='$sid'")" 0
  check "and fails the criterion" eq "$(q "SELECT payload->'results'->0->>'result' FROM tape_events WHERE session_id='$sid' AND kind='verification'")" fail
}

summaries() {
  local ws; ws=$(new_workspace summary)
  start_kernel "$ws" "$(script summary.json)" ZEN_CONTEXT_TOKENS=4000 ZEN_COMPACT_IDLE_SECS=0 ZEN_SUMMARY_MODEL=faux/smoke
  local sid; sid=$(zen ask --json -m faux/smoke "one" | jq -r .session_id)
  zen ask --json -s "$sid" "two" >/dev/null; sleep 1
  for _ in $(seq 1 20); do [ "$(q "SELECT count(*) FROM compactions")" -gt 0 ] && break; sleep 0.5; done
  zen ask --json -s "$sid" "three" >/dev/null
  check "a summary was prepared and applied" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND kind='compaction'")" 1
  check "the turn records it as an expected cache break" eq "$(q "SELECT cache_break FROM turns WHERE session_id='$sid' ORDER BY started_at DESC LIMIT 1")" summary
  check "the summary kept the fact" grep -q E4127 <<<"$(q "SELECT payload->>'text' FROM tape_events WHERE session_id='$sid' AND kind='compaction'")"
  check "the history tool finds summarized messages" grep -q "Tool result (bash): FACT: the error code is E4127" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='history' ORDER BY seq DESC LIMIT 1")"
}

system_one_direct() {
  local ws; ws=$(new_workspace systemone)
  local port=$((PORT + 3))
  python3 "$REPO/scripts/e2e/systemone_stub.py" "$port" & local srv=$!
  start_kernel "$ws" "$(script systemone.json)" ZEN_WORKERS=engine,pi ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key ZEN_S1_URL="http://127.0.0.1:$port/systemone"
  check "a stale pi worker setting is ignored" bash -c '! grep -q "worker `pi` started" "$1"' _ "$TMP/kernel.log"
  local sid; sid=$(zen ask --json -m faux/smoke "decide directly" | jq -r .session_id)
  local answer; answer=$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'toolName'='decide' ORDER BY seq DESC LIMIT 1")
  check "System One bool mapped from noul" grep -q '"probability": 0.82' <<<"$answer"
  check "System One choice and score parsed" bash -c 'grep -q "\"choice\": \"build\"" <<<"$1" && grep -q "\"score\": 1" <<<"$1"' _ "$answer"
  check "the decision was logged" eq "$(q "SELECT count(*) FROM decisions WHERE session_id='$sid' AND point='tool' AND error IS NULL")" 1
  stop_kernel
  start_kernel "$ws" "$(script systemone.json)" ZEN_WORKERS=pi ZEN_S1_MODEL=openrouter/typesafe/jev-1.13 OPENROUTER_API_KEY=e2e-key ZEN_S1_URL="http://127.0.0.1:$port/systemone"
  check "pi-only legacy setting falls back to engine" grep -q 'worker `engine` started' "$TMP/kernel.log"
  kill "$srv" 2>/dev/null || true
}

secrets_masked() {
  local ws; ws=$(new_workspace secret)
  start_kernel "$ws" "$(script secret.json)" E2E_PLANTED_KEY=planted-value-1234567890
  local sid; sid=$(zen ask --json -m faux/smoke "print the token" | jq -r .session_id)
  check "the kernel's secret variables are not in the agent's shell" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult' AND payload::text LIKE '%vars: 0%'")" 1
  local out; out=$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "the token is masked on the tape" grep -q 'ghp_…\[masked\]' <<<"$out"
  # (The command the model typed still contains it: masking applies to what tools return.)
  check "no tool output holds the token's value" eq "$(q "SELECT count(*) FROM tape_events WHERE payload->>'role'='toolResult' AND payload::text LIKE '%AbCdEfGhIjKlMnOp%'")" 0
  check "URL tokens are rejected for HTTP" eq "$(curl -s -o /dev/null -w '%{http_code}' "$URL/api/models?token=$TOKEN")" 401
  check "URL tokens need a WebSocket upgrade" eq "$(curl -s -o /dev/null -w '%{http_code}' "$URL/api/sessions/$sid/ws?token=$TOKEN")" 401
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
  check "the ended turn's late answer was dropped" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND payload::text LIKE '%LATE answer%'")" 0
}

# A session the old briefed workflow left mid-verification still works after an upgrade.
restart_recovery() {
  local ws; ws=$(new_workspace restart)
  start_kernel "$ws" ""
  local sid; sid=$(zen sessions new --json -m faux/smoke | jq -r .id)
  q "UPDATE sessions SET state = 'verifying' WHERE id = '$sid'" >/dev/null
  stop_kernel
  start_kernel "$ws" ""
  check "an old session in a workflow state takes a prompt" eq "$(zen ask --json -s "$sid" "hi" | jq -r .error)" null
}

# A summary made at the hard limit by a summarizer slower than the watchdog's idle limit: the turn
# waits for it instead of being taken for stalled.
slow_summary() {
  local ws; ws=$(new_workspace slow)
  start_kernel "$ws" "$(script summary.json)" ZEN_CONTEXT_TOKENS=4000 ZEN_COMPACT_SOFT=100 ZEN_COMPACT_HARD=0.7 \
    ZEN_SUMMARY_MODEL=slow/summarizer ZEN_SLOW_SECS=12 ZEN_TURN_IDLE_SECS=1 ZEN_WORKERS=engine,slow \
    "ZEN_WORKER_SLOW_CMD=python3 $REPO/scripts/e2e/slow_worker.py"
  local sid; sid=$(zen ask --json -m faux/smoke "one" | jq -r .session_id)
  zen ask --json -s "$sid" "two" >/dev/null
  local r; r=$(zen ask --json -s "$sid" "three")
  check "the turn waited for the summary" eq "$(echo "$r" | jq -r .error)" null
  check "the summary was made by the slow summarizer and applied" eq "$(q "SELECT model FROM compactions WHERE session_id='$sid' AND applied_seq > 0")" slow/summarizer
  check "its tools ran (not interrupted)" eq "$(q "SELECT tool_errors || '/' || outcome FROM turns WHERE session_id='$sid' ORDER BY started_at DESC LIMIT 1")" 0/ok
}

run open-loop open_loop
run restart-recovery restart_recovery
run prompt-files prompt_files
run layout-move layout_move
run skills skills_on_demand
run mcp mcp_tools
run web web_tools
run search search_recall
run wiki wiki_capture
run workshop workshop
run delegation delegation
run memory-across-sessions memory_across_sessions
run memory-sleep memory_sleep
run ask ask_and_gone_tools
run verifier verifier
run summaries summaries
run system-one system_one_direct
run secrets secrets_masked
run slow-summary slow_summary
run stale-turn stale_turn
echo "== tape"
check "every tape is numbered and its hash chain recomputes" tape_is_sound

echo
echo "$PASSED scenario(s) passed, $FAILED check(s) failed"
[ "$FAILED" = 0 ]
