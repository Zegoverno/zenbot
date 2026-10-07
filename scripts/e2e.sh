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
  check "defaults installed" test -s "$zh/SOUL.md" -a -s "$zh/AGENTS.md" -a -s "$zh/skills/work/verify/SKILL.md" -a -s "$zh/skills/work/brief/references/template.md"
  check "the owner's USER.md was kept" grep -q "E2E Owner" "$zh/USER.md"
  local sid; sid=$(zen ask --json -m faux/smoke "hi" | jq -r .session_id)
  local base; base=$(q "SELECT payload->>'text' FROM tape_events WHERE session_id='$sid' AND kind='base'")
  check "SOUL.md is in the instructions" grep -q "<soul" <<<"$base"
  check "AGENTS.md is the environment, placeholders filled" grep -qF "Working directory for tools: $ws" <<<"$base"
  check "USER.md is in the instructions" grep -q "E2E Owner" <<<"$base"
  check "the skills index is in the instructions" grep -q "work/verify:" <<<"$base"
  check "memory starts empty" grep -q "(empty)" <<<"$base"
  local tools; tools=$(q "SELECT string_agg(t->>'name', ',') FROM turns, envelopes e, jsonb_array_elements(e.tools) t WHERE turns.session_id='$sid' AND e.hash = turns.envelope")
  check "the system tools, in order, no workflow tools" eq "$tools" bash,read,write,edit,history,ask,remember,find_skills,load_skill,verify
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

# remember: a memory saved in one session is in the next session's instructions, not the current one's.
memory_across_sessions() {
  local ws; ws=$(new_workspace memory)
  start_kernel "$ws" "$(script agent.json)"
  local s1; s1=$(zen ask --json -m faux/smoke "remember this" | jq -r .session_id)
  check "saved with its source" eq "$(q "SELECT source || '/' || tier FROM memories")" owner/short
  check "exported to MEMORY.md" grep -q "\[m1\] The owner prefers tabs" "$TMP/home/.zenbot/MEMORY.md"
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

secrets_masked() {
  local ws; ws=$(new_workspace secret)
  start_kernel "$ws" "$(script secret.json)"
  local sid; sid=$(zen ask --json -m faux/smoke "print the token" | jq -r .session_id)
  local out; out=$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "the token is masked on the tape" grep -q 'ghp_…\[masked\]' <<<"$out"
  # (The command the model typed still contains it: masking applies to what tools return.)
  check "no tool output holds the token's value" eq "$(q "SELECT count(*) FROM tape_events WHERE payload->>'role'='toolResult' AND payload::text LIKE '%AbCdEfGhIjKlMnOp%'")" 0
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
run skills skills_on_demand
run memory-across-sessions memory_across_sessions
run memory-sleep memory_sleep
run ask ask_and_gone_tools
run verifier verifier
run summaries summaries
run secrets secrets_masked
run slow-summary slow_summary
run stale-turn stale_turn
echo "== tape"
check "every tape is numbered and its hash chain recomputes" tape_is_sound

echo
echo "$PASSED scenario(s) passed, $FAILED check(s) failed"
[ "$FAILED" = 0 ]
