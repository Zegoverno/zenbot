#!/usr/bin/env bash
# Compare two zenbot harness builds on the fixed tasks in evals/tasks, with the same model and
# thinking level. The result is a report for the owner to decide on; it never blocks anything.
#
#   scripts/eval.sh                  this checkout (new) vs the installed version (base), on the
#                                    tasks the change can affect (evals/areas.txt)
#   scripts/eval.sh --base REF       ... vs a commit or branch
#   scripts/eval.sh --plan           print which tasks would run and why, then stop
#
#   --model ID        model for every run (default: the running zenbot's default model)
#   --effort LEVEL    thinking level (default: the model's default)
#   --base-model ID   model for the base runs, when the base names it differently
#   --base-effort L   thinking level for the base runs
#   --tasks a,b       only these tasks (self-tests such as `smoke` run only when named)
#   --full            every task, not just the ones the change affects
#   --repeat N        runs per task and harness (default 1)
#   --jobs N          runs at once (default ZEN_EVAL_JOBS, else 3); 1 runs them one by one
#   --fresh           run the base again even when the cache has its result
#   --only new|base   run one harness only
#   --native ENGINE   the base is the vendor's own CLI (claude or codex) with its own tools, not a
#                     zenbot build: how much zenbot's harness adds or costs (scripts/eval-native.sh);
#                     runs every task unless --tasks names some
#   --keep            keep the per-run databases and workspaces
#
# Which tasks: the files that differ between the base and this checkout (uncommitted ones too) are
# mapped to areas by evals/areas.txt, and the tasks whose `areas` meet them run, plus the `core`
# tasks. When no harness file changed there is nothing to compare, and it says so and stops.
#
# Each run (harness, task, repeat) gets a fresh copy of the task's files as the workspace, its own
# kernel on its own port, its own database (zen_eval_*) and its own zenbot home, so nothing touches
# the live service or its data, and runs at the same time can't see each other's sessions. Base and
# new runs of a task are interleaved, so both sides run under the same conditions. A base run that
# completed is cached in ~/.zenbot/evals/cache/ and reused for ZEN_EVAL_CACHE_DAYS (default 7) days
# while nothing it depends on changed (base commit, model, effort, the task, the engine versions).
# Results and the report go to ~/.zenbot/evals/<run>/.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH"
. "$REPO/scripts/db.sh"

BASE_REF="" MODEL="" EFFORT="" BASE_MODEL="" BASE_EFFORT="" TASKS="" REPEAT=1 ONLY="" KEEP="" NATIVE=""
JOBS=${ZEN_EVAL_JOBS:-3} FRESH="" FULL="" PLAN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE_REF=$2; shift ;;
    --model) MODEL=$2; shift ;;
    --effort) EFFORT=$2; shift ;;
    --base-model) BASE_MODEL=$2; shift ;;
    --base-effort) BASE_EFFORT=$2; shift ;;
    --tasks) TASKS=$2; shift ;;
    --full) FULL=1 ;;
    --plan) PLAN=1 ;;
    --repeat) REPEAT=$2; shift ;;
    --jobs) JOBS=$2; shift ;;
    --fresh) FRESH=1 ;;
    --only) ONLY=$2; shift ;;
    --keep) KEEP=1 ;;
    --native) NATIVE=$2; shift ;;
    -h|--help) sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
[[ $JOBS =~ ^[1-9][0-9]*$ ]] || { echo "--jobs takes a number from 1" >&2; exit 2; }

PORT=${ZEN_EVAL_PORT:-18301}
CACHE="$HOME/.zenbot/evals/cache"
CACHE_DAYS=${ZEN_EVAL_CACHE_DAYS:-7}
RUN="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$HOME/.zenbot/evals/$RUN"
WORK="$OUT/work"
BUILDS="$HOME/.zenbot/evals/builds"
log() { echo "$*" >&2; }

# The base commit: --base, else the installed version.
if [ -z "$NATIVE" ]; then
  BASE_SHA=$(git rev-parse --short=12 "${BASE_REF:-$(cat "$HOME/.zenbot/version" 2>/dev/null || echo HEAD)}^{commit}")
fi

# ---------- which tasks ----------

# Tasks that run without --tasks: every directory in evals/tasks except self-tests.
all_tasks() {
  local t
  for t in evals/tasks/*/task.json; do
    jq -e '.selftest == true' "$t" >/dev/null || basename "$(dirname "$t")"
  done
}

# The areas of a path, from evals/areas.txt: those of every line whose prefix it starts with (`core`
# only when there is no other: core tasks always run).
path_areas() {
  awk -v f="$1" '{ sub(/#.*/, "") } NF > 1 && index(f, $1) == 1 { for (i = 2; i <= NF; i++) print $i }' \
    evals/areas.txt | sort -u | paste -sd' ' | sed -E 's/^core (.)/\1/; s/ core( |$)/\1/'
}

# Fills TASK_LIST and SELECTION (why each task runs, for --plan and the report). Sets NO_CHANGE
# when no harness file differs from the base.
NO_CHANGE=""
select_tasks() {
  local t
  if [ -n "$TASKS" ]; then
    IFS=, read -ra TASK_LIST <<< "$TASKS"
    SELECTION="Tasks named with --tasks: ${TASK_LIST[*]}"
    return
  fi
  mapfile -t TASK_LIST < <(all_tasks)
  if [ -n "$FULL" ] || [ -n "$NATIVE" ]; then
    SELECTION="Every task ($([ -n "$FULL" ] && echo "--full" || echo "--native")): ${#TASK_LIST[@]}"
    return
  fi
  local changed=() harness=() other=() f a want=" core "
  mapfile -t changed < <({ git diff --name-only "$BASE_SHA" --; git ls-files --others --exclude-standard; } | sort -u)
  for f in "${changed[@]}"; do
    a=$(path_areas "$f")
    if [ -n "$a" ]; then harness+=("  $f → $a"); want+="$a "; else other+=("$f"); fi
  done
  SELECTION="Changes against base $BASE_SHA: ${#changed[@]} file(s)"
  if [ ${#harness[@]} -eq 0 ]; then
    NO_CHANGE=1
    [ ${#other[@]} -eq 0 ] || SELECTION+=$'\n'"None is a harness path (evals/areas.txt): ${other[*]:0:8}$([ ${#other[@]} -gt 8 ] && echo " …")"
    TASK_LIST=()
    return
  fi
  SELECTION+=$'\n'"Harness paths (evals/areas.txt):"$'\n'"$(printf '%s\n' "${harness[@]}")"
  [ ${#other[@]} -eq 0 ] || SELECTION+=$'\n'"Not harness: ${#other[@]} file(s)"
  local areas; areas=$(tr ' ' '\n' <<<"$want" | sed '/^$/d' | sort -u | paste -sd' ')
  SELECTION+=$'\n'"Areas: $areas"$'\n'"Tasks:"
  local picked=() skipped=() ta hit x
  for t in "${TASK_LIST[@]}"; do
    ta=$(jq -r '(.areas // []) | join(" ")' "evals/tasks/$t/task.json")
    hit=""
    if [ -z "$ta" ]; then hit="(no areas: always runs)"
    elif [[ $want == *" all "* ]]; then hit="all"
    else for x in $ta; do [[ $want == *" $x "* ]] && hit+="${hit:+ }$x"; done
    fi
    if [ -n "$hit" ]; then picked+=("$t"); SELECTION+=$'\n'"$(printf '  %-28s %s' "$t" "$hit")"; else skipped+=("$t"); fi
  done
  [ ${#skipped[@]} -eq 0 ] || SELECTION+=$'\n'"Not affected (--full runs them too): ${skipped[*]}"
  TASK_LIST=("${picked[@]}")
}

select_tasks
for t in "${TASK_LIST[@]}"; do [ -f "evals/tasks/$t/task.json" ] || { echo "no task: $t" >&2; exit 2; }; done
if [ -n "$PLAN" ]; then
  echo "$SELECTION"
  [ -n "$NO_CHANGE" ] && echo "no harness change; no eval needed"
  [ -n "$NO_CHANGE" ] || echo "${#TASK_LIST[@]} task(s) × $REPEAT run(s), $JOBS at once"
  exit 0
fi
if [ -n "$NO_CHANGE" ]; then
  log "$SELECTION"
  echo "no harness change; no eval needed (--full or --tasks runs one anyway)"
  exit 0
fi

mkdir -p "$WORK" "$BUILDS" "$CACHE"
TOKEN="$(cat "$HOME/.zenbot/token")"
LIVE_PORT=$(zen_env ZEN_PORT); LIVE_PORT=${LIVE_PORT:-8100}
if [ -z "$MODEL" ]; then
  MODEL=$(curl -fs -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$LIVE_PORT/api/models" | jq -r '.default // empty')
  [ -n "$MODEL" ] || { echo "can't ask the running zenbot for its default model; pass --model" >&2; exit 1; }
fi
BASE_MODEL=${BASE_MODEL:-$MODEL}
BASE_EFFORT=${BASE_EFFORT:-$EFFORT}

# ---------- harness builds ----------

# The new harness is this checkout as it is now, uncommitted changes included. Its label says so:
# <commit>, or <commit>+<hash of the diff> when the tree has changes.
build_new() {
  log "== building new harness (this checkout)"
  cargo build --release -q
  mkdir -p "$OUT/new-bin"
  cp target/release/zend target/release/zen-engine target/release/zen "$OUT/new-bin/"
  local label; label=$(git rev-parse --short HEAD)
  if [ -n "$(git status --porcelain -- crates scripts Cargo.toml Cargo.lock)" ]; then
    label="$label+$( (git diff HEAD -- crates scripts Cargo.toml Cargo.lock; git ls-files --others --exclude-standard -- crates scripts | xargs -r cat) | sha1sum | cut -c1-7)"
  fi
  NEW_LABEL=$label
}

# Keep the ZEN_EVAL_KEEP_BUILDS (default 5) most recently used base builds; remove the others
# and their git worktrees.
prune_builds() {
  local d
  { ls -1dt "$BUILDS"/*/ 2>/dev/null || true; } | tail -n +$((${ZEN_EVAL_KEEP_BUILDS:-5} + 1)) | while read -r d; do
    d=${d%/}
    log "== removing old base build $(basename "$d")"
    git worktree remove --force "$d/src" 2>/dev/null || true
    rm -rf "$d"
  done
  git worktree prune
}

# The base harness is a commit, built once and cached in ~/.zenbot/evals/builds/<commit>: the
# binaries CI published for it when there are some, otherwise compiled from a worktree.
build_base() {
  local sha=$BASE_SHA
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
  fi
  touch "$dir"
  prune_builds
  BASE_BIN="$dir/bin"
  BASE_LABEL=$(git rev-parse --short "$sha")
}

# ---------- the base cache ----------

# What a base result depends on besides its harness: the task (task.json, files/, hidden/) and the
# engines' versions.
task_hash() {
  (cd "evals/tasks/$1" && find task.json files hidden \( -type f -o -type l \) 2>/dev/null | LC_ALL=C sort |
    xargs -r -d '\n' sha256sum) | sha256sum | cut -c1-16
}
ENGINE_VERSIONS="claude $(claude --version 2>/dev/null | head -1 || true); codex $(codex --version 2>/dev/null | head -1 || true)"

cache_key() { # task repeat
  local base
  if [ -n "$NATIVE" ]; then base="native $NATIVE $(sha256sum scripts/eval-native.sh | cut -c1-16)"
  else base="commit $(git rev-parse "$BASE_SHA^{commit}")"; fi
  printf '%s\n' "$base" "model $BASE_MODEL" "effort $BASE_EFFORT" "task $1 $(task_hash "$1")" \
    "engines $ENGINE_VERSIONS" "repeat $2" | sha256sum | cut -c1-32
}

# The cached result for a key, if one younger than CACHE_DAYS exists (marked with where it came from).
cache_get() {
  local f="$CACHE/$1.json"
  [ -z "$FRESH" ] && [ -f "$f" ] && [ -n "$(find "$f" -mmin -$((CACHE_DAYS * 1440)) 2>/dev/null)" ] || return 1
  jq -c '.result + {cached: {run: .run, at: .at}}' "$f"
}

cache_put() { # key result-line
  jq -c --arg run "$RUN" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '{run: $run, at: $at, result: .}' <<<"$2" >"$CACHE/$1.json.tmp"
  mv "$CACHE/$1.json.tmp" "$CACHE/$1.json"
}

# ---------- one run ----------

# Every process a run starts is a descendant of a worker; stopping the workers' trees stops them
# all (kernels, engines, CLIs), then the run's leftover databases go.
WORKERS=()
proc_tree() { echo "$1"; local c; for c in $(ps -o pid= --ppid "$1" 2>/dev/null); do proc_tree "$c"; done; }
stop_trees() {
  local pids="" p
  [ $# -gt 0 ] || return 0
  for p in "$@"; do pids+=" $(proc_tree "$p")"; done
  # Freeze first, so nothing starts a new process while the tree is being stopped.
  kill -STOP $pids 2>/dev/null || true
  for p in "$@"; do pids+=" $(proc_tree "$p")"; done
  kill -STOP $pids 2>/dev/null || true
  kill -TERM $pids 2>/dev/null || true
  kill -CONT $pids 2>/dev/null || true
}
cleanup() {
  trap - EXIT INT TERM
  stop_trees "${WORKERS[@]}"
  wait 2>/dev/null || true
  if [ -z "$KEEP" ] && [ -d "$OUT" ]; then
    local db
    for db in $(db_psql -d postgres -c "SELECT datname FROM pg_database WHERE datname LIKE 'zen_eval\_$(echo "$RUN" | tr 'A-Z' 'a-z')\_%'" 2>/dev/null); do
      db_drop "$db"
    done
  fi
}
trap cleanup EXIT
trap 'log "stopping…"; cleanup; exit 130' INT TERM

KERNEL_PID=""
stop_kernel() {
  if [ -n "$KERNEL_PID" ]; then kill "$KERNEL_PID" 2>/dev/null || true; wait "$KERNEL_PID" 2>/dev/null || true; fi
  KERNEL_PID=""
}

start_kernel() { # bin workspace db label log port [task-env…]
  local db_url; db_url=$(db_url_for "$3")
  local port=$6 task_env=("${@:7}")
  (
    set -a; [ -f "$HOME/.zenbot/env" ] && . "$HOME/.zenbot/env"; set +a
    # Settings the task asks for (e.g. a small context budget); a build that doesn't know one ignores it.
    for kv in "${task_env[@]}"; do export "$kv"; done
    # Its own zenbot home: the default prompt files and skills, not the owner's, and memory exports
    # that never touch ~/.zenbot (a build that predates ZEN_HOME ignores it).
    ZEN_JOBS=0 ZEN_TOKEN="$TOKEN" ZEN_PORT=$port ZEN_WORKSPACE="$2" ZEN_HARNESS="$4" ZEN_WORKERS=engine ZEN_FAUX=1 \
      ZEN_ENGINE_CMD="$1/zen-engine" DATABASE_URL="$db_url" ZEN_HOME="$2.zenbot" \
      exec "$1/zend"
  ) >"$5" 2>&1 &
  KERNEL_PID=$!
  wait_healthy "http://127.0.0.1:$port/health" 60 "$KERNEL_PID" && return 0
  log "kernel did not start; see $5"; return 1
}

# Run every step of a task, then its checks; print one JSON result line.
run_task() { # name bin label model effort db task repeat port
  local name=$1 bin=$2 label=$3 model=$4 effort=$5 db=$6 task=$7 r=$8 port=$9
  local tdir="$REPO/evals/tasks/$task" ws="$WORK/$name/$task-$r" turns="$WORK/$name/$task-$r.turns.jsonl"
  mkdir -p "$ws"; : > "$turns"
  [ -d "$tdir/files" ] && cp -a "$tdir/files/." "$ws/"
  local started; started=$(date +%s%3N)
  local error="" sid=""
  local task_env=(); mapfile -t task_env < <(jq -r '.env // {} | to_entries[] | "\(.key)=\(.value)"' "$tdir/task.json")
  local native=""; [[ $bin == native:* ]] && native=${bin#native:}
  if [ -n "$native" ] || start_kernel "$bin" "$ws" "$db" "$label" "$WORK/$name/$task-$r.kernel.log" "$port" "${task_env[@]}"; then
    local n; n=$(jq '.steps | length' "$tdir/task.json")
    for i in $(seq 0 $((n - 1))); do
      local step; step=$(jq -c ".steps[$i]" "$tdir/task.json")
      if echo "$step" | jq -e 'has("shell")' >/dev/null; then
        (cd "$ws" && TASK_DIR="$tdir" bash -c "$(echo "$step" | jq -r .shell)") >>"$WORK/$name/$task-$r.shell.log" 2>&1 || { error="setup step $((i + 1)) failed"; break; }
        continue
      fi
      local prompt; prompt=$(echo "$step" | jq -r .prompt)
      local res t0; t0=$(date +%s%3N)
      if [ -n "$native" ]; then
        res=$(timeout 900 "$REPO/scripts/eval-native.sh" "$native" "$model" "$effort" "$ws" "$prompt" "$sid" 2>/dev/null) || true
      else
        local args=(ask --json)
        if [ -z "$sid" ]; then args+=(-m "$model"); [ -n "$effort" ] && args+=(-e "$effort"); else args+=(-s "$sid"); fi
        res=$(ZEN_URL="http://127.0.0.1:$port" ZEN_TOKEN="$TOKEN" timeout 900 "$OUT/new-bin/zen" "${args[@]}" "$prompt" 2>/dev/null) || true
      fi
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
      if out=$(cd "$ws" && TASK_DIR="$tdir" ANSWER="$answer" TURNS="$turns" timeout 300 bash -c "$(echo "$check" | jq -r .run)" 2>&1); then ok=true; fi
    elif echo "$check" | jq -e 'has("answer_contains") or has("answer_lacks")' >/dev/null; then
      # The answer to one prompt (`step`: 1 = the first prompt), else the last one.
      local text="$answer" step; step=$(echo "$check" | jq -r '.step // empty')
      [ -n "$step" ] && text=$(sed -n "${step}p" "$turns" | jq -r '.text // ""')
      if echo "$check" | jq -e 'has("answer_contains")' >/dev/null; then
        grep -qiF -- "$(echo "$check" | jq -r .answer_contains)" <<<"$text" && ok=true
      else
        grep -qiF -- "$(echo "$check" | jq -r .answer_lacks)" <<<"$text" || ok=true
      fi
    fi
    checks=$(jq -c --arg n "$cname" --argjson ok "$ok" --arg out "$(echo "$out" | tail -c 400)" '. + [{name: $n, ok: $ok, output: $out}]' <<<"$checks")
  done
  jq -cn --arg task "$task" --argjson r "$r" --arg h "$name" --arg label "$label" --arg model "$model" --arg effort "$effort" \
    --arg error "$error" --argjson checks "$checks" --slurpfile turns "$turns" --argjson wall $(( $(date +%s%3N) - started )) \
    '{task: $task, repeat: $r, harness: $h, label: $label, model: $model, effort: (if $effort == "" then null else $effort end),
      error: (if $error == "" then null else $error end), passed: ($error == "" and ($checks | all(.ok))),
      checks: $checks, turns: $turns, wall_ms: $wall}'
  [ -z "$KEEP" ] && rm -rf "$ws" "$ws.zenbot"
  true
}

# One unit of work: harness × task × repeat, on the worker's own port, with its own database. A
# base result comes from the cache when it can, and goes into it when the run completed.
run_unit() { # slot index
  local slot=$1 i=$2 h task r
  read -r h task r <<<"${UNITS[$i]}"
  local bin=${H_BIN[$h]} label=${H_LABEL[$h]} model=${H_MODEL[$h]} effort=${H_EFFORT[$h]}
  local part="$OUT/parts/$i.json" key="" res
  if [ "$h" = base ]; then
    key=$(cache_key "$task" "$r")
    if res=$(cache_get "$key"); then
      echo "$res" >"$part"
      log "   $h · $task · run $r: $(jq -r 'if .passed then "passed" else "FAILED" end' <<<"$res") (cached $(jq -r .cached.at <<<"$res"))"
      return 0
    fi
  fi
  log "-- $h · $task · run $r (port $((PORT + slot)))"
  local db; db="zen_eval_$(echo "$RUN" | tr 'A-Z' 'a-z')_$i"
  [[ $bin == native:* ]] || db_psql -d postgres -c "CREATE DATABASE $db" >/dev/null
  mkdir -p "$WORK/$h"
  res=$(run_task "$h" "$bin" "$label" "$model" "$effort" "$db" "$task" "$r" $((PORT + slot)))
  [ -n "$KEEP" ] || [[ $bin == native:* ]] || db_drop "$db"
  echo "$res" >"$part"
  [ -z "$key" ] || [ "$(jq -r '.error // empty' <<<"$res")" != "" ] || cache_put "$key" "$res"
  log "   $h · $task · run $r: $(jq -r '(if .passed then "passed" else "FAILED" end) + (if .error then " (" + .error + ")" else "" end) + " in \(.wall_ms / 1000 | round)s"' <<<"$res")"
}

# The next unit index from the shared queue, or nothing when all are taken.
next_unit() {
  local n
  { flock 9; n=$(<"$OUT/parts/queue"); echo $((n + 1)) >"$OUT/parts/queue"; } 9>"$OUT/parts/queue.lock"
  [ "$n" -lt "${#UNITS[@]}" ] && echo "$n"
}

worker() { # slot
  local i
  trap - EXIT INT TERM
  while i=$(next_unit); do
    run_unit "$1" "$i" || log "   run ${UNITS[$i]} failed in the runner"
  done
}

# ---------- main ----------

build_new
if [ -n "$NATIVE" ]; then
  [[ $NATIVE == claude || $NATIVE == codex ]] || { echo "--native takes claude or codex" >&2; exit 2; }
  [[ $MODEL == "$NATIVE"/* ]] || { echo "--native $NATIVE needs a $NATIVE/ model (--model)" >&2; exit 2; }
  BASE_BIN="native:$NATIVE"; BASE_LABEL="native $NATIVE $("$NATIVE" --version 2>/dev/null | awk 'NR==1{print ($1 ~ /^[0-9]/) ? $1 : $NF}')"
elif [ "$ONLY" != new ]; then
  build_base
fi
declare -A H_BIN=([new]="$OUT/new-bin" [base]="${BASE_BIN:-}") H_LABEL=([new]="$NEW_LABEL" [base]="${BASE_LABEL:-}")
declare -A H_MODEL=([new]="$MODEL" [base]="$BASE_MODEL") H_EFFORT=([new]="$EFFORT" [base]="$BASE_EFFORT")

# Base and new runs of each task side by side, so both meet the same conditions (load, rate limits).
UNITS=()
for task in "${TASK_LIST[@]}"; do
  for r in $(seq 1 "$REPEAT"); do
    [ "$ONLY" = new ] || UNITS+=("base $task $r")
    [ "$ONLY" = base ] || UNITS+=("new $task $r")
  done
done
mkdir -p "$OUT/parts"; echo 0 >"$OUT/parts/queue"
[ "$JOBS" -le "${#UNITS[@]}" ] || JOBS=${#UNITS[@]}
echo "$SELECTION" >"$OUT/selection.txt"
log "$SELECTION"
log "== model $MODEL${EFFORT:+ · effort $EFFORT} · ${#TASK_LIST[@]} tasks × $REPEAT · $JOBS at once · results in $OUT"
STARTED=$(date +%s)
for slot in $(seq 0 $((JOBS - 1))); do
  worker "$slot" &
  WORKERS+=($!)
done
wait "${WORKERS[@]}" || true
WORKERS=()
log "== ran ${#UNITS[@]} runs in $(( $(date +%s) - STARTED ))s, $JOBS at once"
echo "Ran ${#UNITS[@]} runs in $(( $(date +%s) - STARTED ))s, $JOBS at once." >>"$OUT/selection.txt"

# Results in a fixed order (task, repeat), whatever order the runs finished in.
for h in base new; do
  cat "$OUT"/parts/*.json 2>/dev/null | jq -c --arg h "$h" 'select(.harness == $h)' | jq -sc 'sort_by(.task, .repeat) | .[]' >"$OUT/$h.jsonl"
  [ -s "$OUT/$h.jsonl" ] || rm -f "$OUT/$h.jsonl"
done
[ -n "$KEEP" ] || rm -rf "$OUT/parts"
"$REPO/scripts/eval-report.sh" "$OUT" | tee "$OUT/report.md"
