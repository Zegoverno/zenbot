#!/usr/bin/env python3
"""Paired comparison of an eval run's two harnesses (scripts/eval.sh writes base.jsonl and new.jsonl).

    scripts/eval-paired.py ~/.zenbot/evals/<run> [more runs...]

Each task is a pair (base, new) on the same model, so the per-task difference cancels out how hard
the task is; that is what makes a small number of tasks informative. Reports:
- pass/fail: tasks only one side passed (discordant pairs) and an exact two-sided sign test on them;
- cost, tokens, time and tool calls: the geometric mean of new/base per task, with a 90% bootstrap
  interval. A ratio under 1 means the new harness used less.
Runs given together are pooled (e.g. a pilot and a follow-up batch).
"""
import json, math, random, sys
from collections import defaultdict
from math import comb


def load(dirs):
    runs = defaultdict(lambda: defaultdict(list))
    for d in dirs:
        for side in ("base", "new"):
            try:
                for line in open(f"{d}/{side}.jsonl"):
                    r = json.loads(line)
                    runs[r["task"]][side].append(r)
            except FileNotFoundError:
                pass
    return runs


def totals(r):
    t = [x.get("turn") or {} for x in r.get("turns", [])]
    s = lambda k: sum((x.get(k) or 0) for x in t)
    return {
        "passed": bool(r.get("passed")),
        "cost": s("cost_usd"),
        "tokens": s("input_tokens") + s("output_tokens") + s("cache_read") + s("cache_write"),
        "output": s("output_tokens"),
        "secs": sum((x.get("client_ms") or 0) for x in r.get("turns", [])) / 1000,
        "tools": s("tool_calls") or sum(len(x.get("tools") or []) for x in r.get("turns", [])),
        "label": r.get("label"),
    }


def sign_test(wins, losses):
    n = wins + losses
    if n == 0:
        return 1.0
    k = min(wins, losses)
    return min(1.0, 2 * sum(comb(n, i) for i in range(k + 1)) / 2 ** n)


def geo_ci(ratios, reps=5000, seed=1):
    logs = [math.log(x) for x in ratios if x > 0]
    if not logs:
        return None
    rnd = random.Random(seed)
    means = sorted(sum(rnd.choice(logs) for _ in logs) / len(logs) for _ in range(reps))
    g = lambda v: math.exp(v)
    return g(sum(logs) / len(logs)), g(means[int(reps * 0.05)]), g(means[int(reps * 0.95)])


def main():
    runs = load(sys.argv[1:])
    pairs, labels = [], set()
    print(f"{'task':34} {'base':>5} {'new':>5}  {'cost b→n':>16}  {'secs b→n':>12}  {'tools b→n':>9}")
    for task in sorted(runs):
        b, n = runs[task]["base"], runs[task]["new"]
        for rb, rn in zip(b, n):
            tb, tn = totals(rb), totals(rn)
            labels |= {tb["label"], tn["label"]}
            pairs.append((task, tb, tn))
            print(f"{task:34} {'pass' if tb['passed'] else 'FAIL':>5} {'pass' if tn['passed'] else 'FAIL':>5}  "
                  f"{tb['cost']:7.3f}→{tn['cost']:7.3f}  {tb['secs']:5.0f}→{tn['secs']:5.0f}  {tb['tools']:4}→{tn['tools']:<4}")
    if not pairs:
        print("no paired runs")
        return
    wins = sum(1 for _, b, n in pairs if n["passed"] and not b["passed"])
    losses = sum(1 for _, b, n in pairs if b["passed"] and not n["passed"])
    pb = sum(b["passed"] for _, b, _ in pairs)
    pn = sum(n["passed"] for _, _, n in pairs)
    print(f"\nharnesses: base = {sorted(l for l in labels if l)}")
    print(f"pairs {len(pairs)} · passed base {pb}, new {pn} · only new passed {wins}, only base passed {losses} "
          f"· sign test p = {sign_test(wins, losses):.2f}")
    for key, name in (("cost", "cost"), ("tokens", "tokens"), ("output", "output tokens"), ("secs", "time"), ("tools", "tool calls")):
        ratios = [n[key] / b[key] for _, b, n in pairs if b[key] and n[key]]
        ci = geo_ci(ratios)
        if ci:
            print(f"{name:14} new/base geometric mean {ci[0]:.2f}  (90% CI {ci[1]:.2f}–{ci[2]:.2f}, {len(ratios)} pairs)")


if __name__ == "__main__":
    main()
