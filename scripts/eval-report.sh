#!/usr/bin/env bash
# Print the comparison report for an eval run: scripts/eval-report.sh ~/.zenbot/evals/<run>
# Reads base.jsonl and new.jsonl (either may be missing) written by scripts/eval.sh.
set -euo pipefail
OUT=${1:?usage: eval-report.sh <run directory>}
cat "$OUT"/base.jsonl "$OUT"/new.jsonl 2>/dev/null | jq -rs '
  # Totals of one run, from the kernel turn records (older kernels send none: then the counts the client saw).
  def totals: {
    passed,
    cost: ([.turns[] | .turn.cost_usd // .usage.cost_usd_api_equivalent // 0] | add // 0),
    tokens: ([.turns[] | if .turn then (.turn.input_tokens + .turn.output_tokens + .turn.cache_read + .turn.cache_write)
                         else (.usage.input_tokens + .usage.output_tokens) end] | add // 0),
    cache_read: ([.turns[] | .turn.cache_read // empty] | add),
    cache_in: ([.turns[] | .turn | select(.) | .input_tokens + .cache_read + .cache_write] | add),
    secs: (([.turns[] | .client_ms // 0] | add // 0) / 1000),
    tool_errors: ([.turns[] | .turn.tool_errors // 0] | add // 0),
    turns: (.turns | length)
  };
  def mean(f): if length == 0 then null else (map(f) | add) / length end;
  def summary: {
    runs: length,
    passed: (map(select(.passed)) | length),
    cost: mean(.cost), tokens: mean(.tokens), secs: mean(.secs), tool_errors: mean(.tool_errors),
    cache: (if any(.cache_in != null) then ((map(.cache_read // 0) | add) / ([map(.cache_in // 0) | add, 1] | max)) else null end)
  };
  def money: if . == null then "–" else "$" + ((. * 1000 | round) / 1000 | tostring) end;
  def kilo: if . == null then "–" elif . >= 1000 then ((. / 100 | round) / 10 | tostring) + "k" else (round | tostring) end;
  def pct: if . == null then "–" else ((. * 100 | round) | tostring) + "%" end;
  def secs: if . == null then "–" else ((. | round) | tostring) + "s" end;
  def change(a; b): if a == null or b == null or a == 0 then "" else
    ((b - a) / a * 100 | round) as $p | " (" + (if $p > 0 then "+" else "" end) + ($p | tostring) + "%)" end;
  def pair(f; fmt): (.base | f | fmt) + " → " + (.new | f | fmt) + change(.base | f; .new | f);

  . as $all
  | ($all | map(select(.harness == "base")) | first) as $b
  | ($all | map(select(.harness == "new")) | first) as $n
  | (if $b and $n then "both" elif $n then "new" else "base" end) as $mode
  | ([$all[] | .turns[] | .turn | select(.) | "\(.engine) \(.engine_version)"] | unique) as $engines_all
  | (reduce ($all | group_by(.task))[] as $g ({}; . + { ($g[0].task): {
        base: ($g | map(select(.harness == "base") | totals) | summary),
        new: ($g | map(select(.harness == "new") | totals) | summary) } })) as $tasks
  | {base: ($all | map(select(.harness == "base") | totals) | summary),
     new: ($all | map(select(.harness == "new") | totals) | summary)} as $total
  # Regressions first: fewer passes, then cost or time up by more than 20%.
  | ($tasks | to_entries | map(.value as $v | . + {regressed: (
        ($v.new.passed < $v.base.passed)
        or (($v.base.cost // 0) > 0 and ($v.new.cost // 0) > $v.base.cost * 1.2)
        or (($v.base.secs // 0) > 0 and ($v.new.secs // 0) > $v.base.secs * 1.2))})
      | sort_by(if .regressed then 0 else 1 end, .key)) as $rows
  | [
      (if $mode == "both" then "# Eval: \($n.label) (new) vs \($b.label) (base)" else "# Eval: \(($n // $b).label)" end),
      "",
      # The level that ran, from the turn records (older kernels record none).
      ([$all[] | select(.harness == "new") | .turns[] | .turn.effort // empty] | unique | join(",")) as $ne
      | ([$all[] | select(.harness == "base") | .turns[] | .turn.effort // empty] | unique | join(",")) as $be
      | "model \(($n // $b).model) · effort " + (if $mode == "both" then "\(if $be == "" then "not recorded" else $be end) → \(if $ne == "" then "not recorded" else $ne end)" else ($ne + $be) end)
        + (if $b and $n and $b.model != $n.model then " (base: \($b.model))" else "" end)
        + " · \($rows | length) tasks × \($all | map(.repeat) | max) run(s)",
      "engines: " + ($engines_all | join(", "))
        + (if ($engines_all | map(split(" ")[0]) | unique | length) < ($engines_all | length) then "  ⚠ engine versions differ between runs; the comparison is not clean" else "" end),
      "",
      (if $mode == "both" then
        "| task | passed | cost | tokens | cache hit | time | tool errors |",
        "|---|---|---|---|---|---|---|",
        ($rows[] | .value as $v | "| \(if .regressed then "⚠ " else "" end)\(.key) | \($v.base.passed)/\($v.base.runs) → \($v.new.passed)/\($v.new.runs) | "
          + ($v | pair(.cost; money)) + " | " + ($v | pair(.tokens; kilo)) + " | " + ($v | pair(.cache; pct)) + " | "
          + ($v | pair(.secs; secs)) + " | " + ($v | pair(.tool_errors; kilo)) + " |"),
        "| **total** | \($total.base.passed)/\($total.base.runs) → \($total.new.passed)/\($total.new.runs) | "
          + ($total | pair(.cost; money)) + " | " + ($total | pair(.tokens; kilo)) + " | " + ($total | pair(.cache; pct)) + " | "
          + ($total | pair(.secs; secs)) + " | " + ($total | pair(.tool_errors; kilo)) + " |",
        "",
        (if ([$all[] | select(.harness == "base") | .turns[] | .turn | select(.)] | length) == 0 then
          "Base has no turn records (built before tracing): its tokens are the counts its stream showed, which miss output and side calls, so compare cost instead; its cache hit is unknown.\n" else empty end),
        "Cost, tokens, cache hit, time (summed over the turns, as the client saw them) and tool errors are means per run. ⚠ marks a task that passed less often, or got over 20% slower or more expensive."
      else
        ($total | if $mode == "new" then .new else .base end) as $t |
        "| task | passed | cost | tokens | cache hit | time | tool errors |",
        "|---|---|---|---|---|---|---|",
        ($rows[] | .value | (if $mode == "new" then .new else .base end) as $v | "| \(.key // "") | \($v.passed)/\($v.runs) | \($v.cost | money) | \($v.tokens | kilo) | \($v.cache | pct) | \($v.secs | secs) | \($v.tool_errors | kilo) |"),
        "| **total** | \($t.passed)/\($t.runs) | \($t.cost | money) | \($t.tokens | kilo) | \($t.cache | pct) | \($t.secs | secs) | \($t.tool_errors | kilo) |"
      end),
      "",
      "Failed checks:",
      ([$all[] | select(.passed | not) | "- \(.harness) · \(.task) run \(.repeat): " +
         (if .error then .error else ([.checks[] | select(.ok | not) | .name] | join(", ")) end)] | if length == 0 then ["- none"] else . end | .[])
    ] | .[]
'
echo
echo "Details: $OUT/{base,new}.jsonl"
