#!/usr/bin/env bash
# End-to-end tests of the kernel with the scripted faux model: each scenario runs real turns through
# a kernel built from this checkout, on a throwaway database and workspace, and checks the result in
# the database. No subscription is used and the live service isn't touched.
#
#   scripts/e2e.sh              build, then run every scenario
#   scripts/e2e.sh workflow     only scenarios whose name contains "workflow"
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
      ZEN_ENGINE_CMD="$BIN/zen-engine" ZEN_VERIFY_SAMPLE=0 DATABASE_URL="$DB_URL" HOME="$TMP/home"
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
  start_kernel "$ws" "" ZEN_BRIEFS=0
  local r; r=$(zen ask --json -m faux/smoke "run the smoke command")
  check "answer" eq "$(echo "$r" | jq -r .error)" null
  check "the bash tool ran" eq "$(echo "$r" | jq -r '[.tools[].name] | join(",")')" bash
  check "state is open" eq "$(q "SELECT state FROM sessions")" open
}

workflow_pass() {
  local ws; ws=$(new_workspace pass)
  start_kernel "$ws" "$(script workflow.json 's/ROUTE/bounded/' 's/TARGET/done.txt/')" ZEN_BRIEFS=always
  local r sid; r=$(zen ask --json -m faux/smoke "Please create done.txt"); sid=$(echo "$r" | jq -r .session_id)
  check "no error" eq "$(echo "$r" | jq -r .error)" null
  check "framing refused the write" eq "$(echo "$r" | jq -r '.tools[0].is_error')" true
  check "the read-only shell blocked the write" test ! -e "$ws/sneaky2.txt"
  check "nothing written while framing" test ! -e "$ws/sneaky.txt"
  check "the work wrote done.txt" test -f "$ws/done.txt"
  check "states: framing, working, verifying, reported, closed" eq "$(q "SELECT string_agg(payload->>'state', ',' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND kind='state'")" working,verifying,reported,closed
  check "criterion passed, verifier skipped" eq "$(q "SELECT payload->'results'->0->>'result' || ' ' || (payload->>'verifier') FROM tape_events WHERE session_id='$sid' AND kind='verification'")" "pass skipped: every criterion is a passing command"
  check "model verdict recorded as the model's" eq "$(q "SELECT decision || '/' || source FROM session_decisions WHERE session_id='$sid'")" accept/model
  check "one envelope for the whole session" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND kind='envelope'")" 1
  check "the report reached the client" grep -q "1 passed" <<<"$(echo "$r" | jq -r .text)"
}

workflow_approval_and_rounds() {
  local ws; ws=$(new_workspace rounds)
  start_kernel "$ws" "$(script workflow.json 's/ROUTE/architectural/' 's/TARGET/never.txt/')" ZEN_VERIFY_ROUNDS=1 ZEN_BRIEFS=always
  local r sid; r=$(zen ask --json -m faux/smoke "Please create never.txt"); sid=$(echo "$r" | jq -r .session_id)
  check "an architectural brief waits for approval" eq "$(q "SELECT state FROM sessions WHERE id='$sid'")" framing
  check "nothing was done before approval" test ! -e "$ws/done.txt"
  r=$(zen ask --json -s "$sid" "go")
  check "approved by the owner" eq "$(q "SELECT payload->>'by' FROM tape_events WHERE session_id='$sid' AND kind='approval'")" owner
  check "a failed check went back to work once, then was reported" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND kind='verification'")" 2
  check "reported, not closed (architectural)" eq "$(q "SELECT state FROM sessions WHERE id='$sid'")" reported
  check "no model verdict for architectural work" eq "$(q "SELECT count(*) FROM session_decisions WHERE session_id='$sid'")" 0
  check "the report shows the failed check" grep -q "check failed" <<<"$(echo "$r" | jq -r .text)"
  # One session is one job: a reply after the report continues it on the same brief.
  zen ask --json -s "$sid" "try again" >/dev/null || true
  check "a reply continues the same job" eq "$(q "SELECT count(*) FROM tape_events WHERE session_id='$sid' AND kind='brief'")" 1
  check "the reply went back to work" eq "$(q "SELECT payload->>'reason' FROM tape_events WHERE session_id='$sid' AND kind='state' AND payload->>'by'='owner' ORDER BY seq DESC LIMIT 1")" "owner continued the job"
}

opt_in() {
  local ws; ws=$(new_workspace optin)
  start_kernel "$ws" "$(script judgment.json)"
  local sid; sid=$(zen sessions new -m faux/smoke --json | jq -r .id)
  check "a session starts open (briefs are opt-in)" eq "$(q "SELECT state FROM sessions WHERE id='$sid'")" open
  zen ask --json -s "$sid" "Please create done.txt" >/dev/null
  check "the model opted in: brief, then work" eq "$(q "SELECT string_agg(payload->>'state' || '/' || (payload->>'by'), ',' ORDER BY seq) FROM tape_events WHERE session_id='$sid' AND kind='state'")" "framing/model,working/auto,verifying/kernel,reported/kernel"
  check "the work was done" test -f "$ws/done.txt"
  check "the open phase is in the turn context" grep -q "Phase: open" <<<"$(q "SELECT payload->>'context' FROM tape_events WHERE session_id='$sid' AND payload->>'role'='user' ORDER BY seq LIMIT 1")"
}

verifier() {
  local ws; ws=$(new_workspace verifier)
  start_kernel "$ws" "$(script judgment.json)" ZEN_BRIEFS=always
  local sid; sid=$(zen ask --json -m faux/smoke "Please create done.txt" | jq -r .session_id)
  local child; child=$(q "SELECT id FROM sessions WHERE parent='$sid' AND kind='verifier'")
  check "a criterion needing judgment ran the verifier" test -n "$child"
  check "the verifier works in the brief's repository" grep -q "^$ws" <<<"$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$child' AND payload->>'toolName'='bash'")"
  check "its judgment is in the verification" eq "$(q "SELECT payload->'results'->0->>'result' FROM tape_events WHERE session_id='$sid' AND kind='verification'")" uncertain
  check "uncertain work waits for the owner (no model verdict)" eq "$(q "SELECT state FROM sessions WHERE id='$sid'")" reported
}

summaries() {
  local ws; ws=$(new_workspace summary)
  start_kernel "$ws" "$(script summary.json)" ZEN_BRIEFS=0 ZEN_CONTEXT_TOKENS=4000 ZEN_COMPACT_IDLE_SECS=0 ZEN_SUMMARY_MODEL=faux/smoke
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
  start_kernel "$ws" "$(script secret.json)" ZEN_BRIEFS=0
  local sid; sid=$(zen ask --json -m faux/smoke "print the token" | jq -r .session_id)
  local out; out=$(q "SELECT payload->'content'->0->>'text' FROM tape_events WHERE session_id='$sid' AND payload->>'role'='toolResult'")
  check "the token is masked on the tape" grep -q 'ghp_…\[masked\]' <<<"$out"
  # (The command the model typed still contains it: masking applies to what tools return.)
  check "no tool output holds the token's value" eq "$(q "SELECT count(*) FROM tape_events WHERE payload->>'role'='toolResult' AND payload::text LIKE '%AbCdEfGhIjKlMnOp%'")" 0
}

restart_recovery() {
  local ws; ws=$(new_workspace restart)
  start_kernel "$ws" ""
  local sid; sid=$(zen sessions new --json | jq -r .id)
  q "UPDATE sessions SET state = 'verifying' WHERE id = '$sid'" >/dev/null
  stop_kernel
  start_kernel "$ws" ""
  check "a verification cut short by a restart goes back to work" eq "$(q "SELECT state FROM sessions WHERE id='$sid'")" working
}

run open-loop open_loop
run restart-recovery restart_recovery
run workflow-pass workflow_pass
run workflow-approval-and-rounds workflow_approval_and_rounds
run opt-in opt_in
run verifier verifier
run summaries summaries
run secrets secrets_masked
echo "== tape"
check "every tape is numbered and its hash chain recomputes" tape_is_sound

echo
echo "$PASSED scenario(s) passed, $FAILED check(s) failed"
[ "$FAILED" = 0 ]
