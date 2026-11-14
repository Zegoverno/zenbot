#!/usr/bin/env bash
# One turn of an eval task through a model vendor's own CLI, with its own tools, for comparing
# zenbot's harness with the native one (scripts/eval.sh --native claude|codex). Prints one JSON line
# shaped like `zen ask --json`: {session_id, text, tools, turn: {...}}, so eval-report.sh reads both.
#
#   scripts/eval-native.sh ENGINE MODEL EFFORT WORKSPACE PROMPT [SESSION]
#
# MODEL is zenbot's id (claude/claude-opus-5-5, codex/gpt-6-sol); EFFORT may be empty (the engine's
# default as zen-engine uses it: medium for Claude, the model's default for Codex). SESSION continues
# an earlier turn. Runs with every permission granted, so only ever in a throwaway workspace. The
# user's own settings, hooks and project files are left out for Claude (`--setting-sources ""`).
set -euo pipefail
ENGINE=$1 MODEL=${2#*/} EFFORT=$3 WS=$4 PROMPT=$5 SESSION=${6:-}
cd "$WS"
# The raw event stream is kept next to the workspace, for reading what the agent did.
EV="$WS.native-$(date +%s%3N).jsonl"
VERSION=$("$ENGINE" --version 2>/dev/null | awk 'NR==1{print ($1 ~ /^[0-9]/) ? $1 : $NF}')

case $ENGINE in
  claude)
    args=(-p --output-format stream-json --verbose --model "$MODEL" --effort "${EFFORT:-medium}"
          --dangerously-skip-permissions --setting-sources "")
    [ -n "$SESSION" ] && args+=(--resume "$SESSION")
    claude "${args[@]}" "$PROMPT" < /dev/null > "$EV" 2>/dev/null || true
    jq -sc --arg v "$VERSION" --arg m "claude/$MODEL" --arg e "${EFFORT:-medium}" '
      (map(select(.type == "result")) | last) as $r
      | [.[] | select(.type == "assistant") | .message.content[]? | select(.type == "tool_use") | {id, name, args: .input}] as $tools
      | ([.[] | select(.type == "user") | .message.content[]? | select(.type == "tool_result" and .is_error == true)] | length) as $errs
      | if $r == null then {error: "no result"} else
        {session_id: $r.session_id, text: ($r.result // ""), tools: $tools,
         turn: {engine: "claude-native", engine_version: $v, model: $m, effort: $e,
                cost_usd: $r.total_cost_usd, input_tokens: $r.usage.input_tokens,
                output_tokens: $r.usage.output_tokens, cache_read: $r.usage.cache_read_input_tokens,
                cache_write: $r.usage.cache_creation_input_tokens, model_calls: $r.num_turns,
                tool_calls: ($tools | length), tool_errors: $errs}} end' "$EV"
    ;;
  codex)
    args=(exec)
    [ -n "$SESSION" ] && args+=(resume "$SESSION")
    args+=(--json --skip-git-repo-check -m "$MODEL" --dangerously-bypass-approvals-and-sandbox)
    [ -n "$EFFORT" ] && args+=(-c "model_reasoning_effort=$EFFORT")
    codex "${args[@]}" "$PROMPT" < /dev/null > "$EV" 2>/dev/null || true
    jq -sc --arg v "$VERSION" --arg m "codex/$MODEL" --arg e "$EFFORT" --arg sid "$SESSION" '
      ([.[] | select(.type == "thread.started") | .thread_id] | first // $sid) as $id
      | [.[] | select(.type == "item.completed") | .item] as $items
      | [$items[] | select(.type != "agent_message" and .type != "reasoning") | {id, name: .type, args: (.command // .changes // null)}] as $tools
      | ([$items[] | select(.type == "command_execution" and (.exit_code // 0) != 0)] | length) as $errs
      | ([.[] | select(.type == "turn.completed") | .usage] | last) as $u
      | if $u == null then {error: "no turn.completed"} else
        {session_id: $id, text: ([$items[] | select(.type == "agent_message") | .text] | last // ""), tools: $tools,
         turn: {engine: "codex-native", engine_version: $v, model: $m, effort: (if $e == "" then null else $e end),
                cost_usd: 0, input_tokens: ($u.input_tokens - ($u.cached_input_tokens // 0)),
                output_tokens: $u.output_tokens, cache_read: ($u.cached_input_tokens // 0), cache_write: 0,
                tool_calls: ($tools | length), tool_errors: $errs}} end' "$EV"
    ;;
  *) echo "engine must be claude or codex" >&2; exit 2 ;;
esac
