#!/usr/bin/env bash
# Compare two zenbot harness builds on the fixed tasks in evals/tasks, with the same model and
# thinking level. The result is a report for the owner to decide on; it never blocks anything.
#
#   scripts/eval.sh                  this checkout (new) vs the installed version (base)
#   scripts/eval.sh --base REF       ... vs a commit or branch
#
#   --model ID        model for every run (default: the running zenbot's default model)
#   --effort LEVEL    thinking level (default: the model's default)
#   --base-model ID   model for the base runs, when the base names it differently
#   --base-effort L   thinking level for the base runs
#   --tasks a,b       only these tasks (self-tests such as `smoke` run only when named)
#   --repeat N        runs per task and harness (default 1)
#   --only new|base   run one harness only
#   --keep            keep the per-run databases and workspaces
#
# Each run gets a fresh copy of the task's files as the workspace, its own kernel and its own
# database (zen_eval_*), so nothing touches the live service or its data. Results and the report
# go to ~/.zenbot/evals/<run>/.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"

BASE_REF="" MODEL="" EFFORT="" BASE_MODEL="" BASE_EFFORT="" TASKS="" REPEAT=1 ONLY="" KEEP=""
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE_REF=$2; shift ;;
    --model) MODEL=$2; shift ;;
    --effort) EFFORT=$2; shift ;;
    --base-model) BASE_MODEL=$2; shift ;;
    --base-effort) BASE_EFFORT=$2; shift ;;
    --tasks) TASKS=$2; shift ;;
    --repeat) REPEAT=$2; shift ;;
    --only) ONLY=$2; shift ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

TOKEN="$(cat "$HOME/.zenbot/token")"
LIVE_PORT=$(grep -E '^ZEN_PORT=' "$HOME/.zenbot/env" 2>/dev/null | cut -d= -f2); LIVE_PORT=${LIVE_PORT:-8100}
PORT=${ZEN_EVAL_PORT:-18301}
RUN="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$HOME/.zenbot/evals/$RUN"
WORK="$OUT/work"
BUILDS="$HOME/.zenbot/evals/builds"
mkdir -p "$WORK" "$BUILDS"
psql() { docker compose -f "$REPO/deploy/compose.yaml" exec -T postgres psql -U zen -d zen -v ON_ERROR_STOP=1 -q "$@"; }
log() { echo "$*" >&2; }

if [ -z "$MODEL" ]; then
  MODEL=$(curl -fs -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$LIVE_PORT/api/models" | jq -r '.default // empty')
  [ -n "$MODEL" ] || { echo "can't ask the running zenbot for its default model; pass --model" >&2; exit 1; }
fi
BASE_MODEL=${BASE_MODEL:-$MODEL}
BASE_EFFORT=${BASE_EFFORT:-$EFFORT}

# Tasks: every directory in evals/tasks except self-tests, or the ones named.
if [ -n "$TASKS" ]; then
  IFS=, read -ra TASK_LIST <<< "$TASKS"
else
  TASK_LIST=()
  for t in evals/tasks/*/task.json; do
    jq -e '.selftest == true' "$t" >/dev/null || TASK_LIST+=("$(basename "$(dirname "$t")")")
  done
fi
for t in "${TASK_LIST[@]}"; do [ -f "evals/tasks/$t/task.json" ] || { echo "no task: $t" >&2; exit 2; }; done

# ---------- harness builds ----------

# The new harness is this checkout as it is now, uncommitted changes included. Its label says so:
# <commit>, or <commit>+<hash of the diff> when the tree has changes.
build_new() {
  log "== building new harness (this checkout)"
  cargo build --release -q
  mkdir -p "$OUT/new-bin"
  cp target/release/zend target/release/zen-engine target/release/zen "$OUT/new-bin/"
  local label; label=$(git rev-parse --short HEAD)
  if [ -n "$(git status --porcelain -- crates packages scripts Cargo.toml Cargo.lock)" ]; then
    label="$label+$( (git diff HEAD -- crates packages scripts Cargo.toml Cargo.lock; git ls-files --others --exclude-standard -- crates packages scripts | xargs -r cat) | sha1sum | cut -c1-7)"
  fi
  NEW_LABEL=$label
}

# The base harness is a commit, built once and cached in ~/.zenbot/evals/builds/<commit>: the
# binaries CI published for it when there are some, otherwise compiled from a worktree.
build_base() {
  local ref=${BASE_REF:-$(cat "$HOME/.zenbot/version" 2>/dev/null || echo HEAD)}
  local sha; sha=$(git rev-parse --short=12 "$ref^{commit}")
  local dir="$BUILDS/$sha"
  if [ ! -x "$dir/bin/zend" ]; then
    log "== building base harness $sha"
    rm -rf "$dir"; git worktree prune
    git worktree add --detach --force "$dir/src" "$sha" >/dev/null 2>&1
    if ! (cd "$dir/src" && ./scripts/fetch-release.sh) 2>/dev/null; then
      (cd "$dir/src" && CARGO_TARGET_DIR="$REPO/target/eval-base" cargo build --release -q)
      mkdir -p "$dir/src/target/release"
      cp "$REPO/target/eval-base/release/zend" "$REPO/target/eval-base/release/zen-engine" "$dir/src/target/release/"
    fi
    mkdir -p "$dir/bin"
    cp "$dir/src/target/release/zend" "$dir/src/target/release/zen-engine" "$dir/bin/"
    [ -d "$REPO/packages/mind/node_modules" ] && ln -sfn "$REPO/packages/mind/node_modules" "$dir/src/packages/mind/node_modules"
  fi
  BASE_BIN="$dir/bin"
  BASE_MIND="$dir/src/packages/mind"
  BASE_LABEL=$(git rev-parse --short "$sha")
}

# ---------- one run ----------

KERNEL_PID=""
stop_kernel() {
  if [ -n "$KERNEL_PID" ]; then kill "$KERNEL_PID" 2>/dev/null || true; wait "$KERNEL_PID" 2>/dev/null || true; fi
  KERNEL_PID=""
}
trap stop_kernel EXIT

start_kernel() { # bin mind_dir workspace db label model log [task-env…]
  local workers=engine
  case "$6" in openai/*) workers=engine,pi ;; esac
  local task_env=("${@:8}")
  (
    set -a; [ -f "$HOME/.zenbot/env" ] && . "$HOME/.zenbot/env"; set +a
    # Settings the task asks for (e.g. a small context budget); a build that doesn't know one ignores it.
    for kv in "${task_env[@]}"; do export "$kv"; done
    ZEN_TOKEN="$TOKEN" ZEN_PORT=$PORT ZEN_WORKSPACE="$3" ZEN_HARNESS="$5" ZEN_WORKERS=$workers ZEN_FAUX=1 \
      ZEN_ENGINE_CMD="$1/zen-engine" ZEN_MIND_DIR="$2" DATABASE_URL="postgres://zen:zen@127.0.0.1:5432/$4" \
      exec "$1/zend"
  ) >"$7" 2>&1 &
  KERNEL_PID=$!
  for _ in $(seq 1 60); do
    curl -fs "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q '"ok":true' && return 0
    kill -0 "$KERNEL_PID" 2>/dev/null || break
    sleep 1
  done
  log "kernel did not start; see $7"; return 1
}

# Run every step of a task, then its checks; print one JSON result line.
run_task() { # name bin mind label model effort db task repeat
  local name=$1 bin=$2 mind=$3 label=$4 model=$5 effort=$6 db=$7 task=$8 r=$9
  local tdir="$REPO/evals/tasks/$task" ws="$WORK/$name/$task-$r" turns="$WORK/$name/$task-$r.turns.jsonl"
  mkdir -p "$ws"; : > "$turns"
  [ -d "$tdir/files" ] && cp -a "$tdir/files/." "$ws/"
  local started; started=$(date +%s%3N)
  local error="" sid=""
  local task_env=(); mapfile -t task_env < <(jq -r '.env // {} | to_entries[] | "\(.key)=\(.value)"' "$tdir/task.json")
  if start_kernel "$bin" "$mind" "$ws" "$db" "$label" "$model" "$WORK/$name/$task-$r.kernel.log" "${task_env[@]}"; then
    local n; n=$(jq '.steps | length' "$tdir/task.json")
    for i in $(seq 0 $((n - 1))); do
      local step; step=$(jq -c ".steps[$i]" "$tdir/task.json")
      if echo "$step" | jq -e 'has("shell")' >/dev/null; then
        (cd "$ws" && bash -c "$(echo "$step" | jq -r .shell)") >>"$WORK/$name/$task-$r.shell.log" 2>&1 || { error="setup step $((i + 1)) failed"; break; }
        continue
      fi
      local prompt; prompt=$(echo "$step" | jq -r .prompt)
      local args=(ask --json)
      if [ -z "$sid" ]; then args+=(-m "$model"); [ -n "$effort" ] && args+=(-e "$effort"); else args+=(-s "$sid"); fi
      local res t0; t0=$(date +%s%3N)
      res=$(ZEN_URL="http://127.0.0.1:$PORT" ZEN_TOKEN="$TOKEN" timeout 900 "$OUT/new-bin/zen" "${args[@]}" "$prompt" 2>/dev/null) || true
      if ! echo "$res" | jq -e .session_id >/dev/null 2>&1; then error="turn $((i + 1)) did not complete"; break; fi
      # Time the turn here, the same way for every harness (older kernels report no durations).
      echo "$res" | jq -c --argjson ms $(( $(date +%s%3N) - t0 )) '. + {client_ms: $ms}' >>"$turns"
      sid=$(echo "$res" | jq -r .session_id)
    done
  else
    error="kernel did not start"
  fi
  stop_kernel
  local answer=""; [ -s "$turns" ] && answer=$(tail -1 "$turns" | jq -r '.text // ""')
  local checks="[]"
  local c; c=$(jq '.checks | length' "$tdir/task.json")
  for i in $(seq 0 $((c - 1))); do
    local check; check=$(jq -c ".checks[$i]" "$tdir/task.json")
    local cname; cname=$(echo "$check" | jq -r .name)
    local ok=false out=""
    if [ -n "$error" ]; then
      out="not run: $error"
    elif echo "$check" | jq -e 'has("run")' >/dev/null; then
      if out=$(cd "$ws" && TASK_DIR="$tdir" timeout 300 bash -c "$(echo "$check" | jq -r .run)" 2>&1); then ok=true; fi
    elif echo "$check" | jq -e 'has("answer_contains")' >/dev/null; then
      grep -qiF -- "$(echo "$check" | jq -r .answer_contains)" <<<"$answer" && ok=true
    fi
    checks=$(jq -c --arg n "$cname" --argjson ok "$ok" --arg out "$(echo "$out" | tail -c 400)" '. + [{name: $n, ok: $ok, output: $out}]' <<<"$checks")
  done
  jq -cn --arg task "$task" --argjson r "$r" --arg h "$name" --arg label "$label" --arg model "$model" --arg effort "$effort" \
    --arg error "$error" --argjson checks "$checks" --slurpfile turns "$turns" --argjson wall $(( $(date +%s%3N) - started )) \
    '{task: $task, repeat: $r, harness: $h, label: $label, model: $model, effort: (if $effort == "" then null else $effort end),
      error: (if $error == "" then null else $error end), passed: ($error == "" and ($checks | all(.ok))),
      checks: $checks, turns: $turns, wall_ms: $wall}'
  [ -z "$KEEP" ] && rm -rf "$ws"
  true
}

run_harness() { # name bin mind label model effort
  local name=$1 db; db="zen_eval_$(echo "${RUN}_$1" | tr 'A-Z' 'a-z')"
  psql -c "CREATE DATABASE $db" >/dev/null
  mkdir -p "$WORK/$name"
  for task in "${TASK_LIST[@]}"; do
    for r in $(seq 1 "$REPEAT"); do
      log "-- $name · $task · run $r"
      run_task "$name" "$2" "$3" "$4" "$5" "$6" "$db" "$task" "$r" | tee -a "$OUT/$name.jsonl" | jq -r '"   " + (if .passed then "passed" else "FAILED" end) + (if .error then " (" + .error + ")" else "" end)' >&2
    done
  done
  [ -z "$KEEP" ] && psql -c "DROP DATABASE $db" >/dev/null
  true
}

# ---------- main ----------

build_new
[ "$ONLY" = new ] || build_base
log "== model $MODEL${EFFORT:+ · effort $EFFORT} · ${#TASK_LIST[@]} tasks × $REPEAT · results in $OUT"
[ "$ONLY" = new ] || run_harness base "$BASE_BIN" "$BASE_MIND" "$BASE_LABEL" "$BASE_MODEL" "$BASE_EFFORT"
[ "$ONLY" = base ] || run_harness new "$OUT/new-bin" "$REPO/packages/mind" "$NEW_LABEL" "$MODEL" "$EFFORT"
"$REPO/scripts/eval-report.sh" "$OUT" | tee "$OUT/report.md"
