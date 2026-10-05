# Evals

Fixed tasks for comparing two versions of zenbot's harness with the same model and thinking
level. Run them before a harness change, and show the report to the owner, who decides. They never
block a commit or an upgrade on their own.

```bash
scripts/eval.sh                                   # this checkout vs the installed version
scripts/eval.sh --base main --tasks repo-question --repeat 3
scripts/eval.sh --model claude/claude-sonnet-5-5 --effort low
scripts/eval.sh --model faux/smoke --tasks smoke  # self-test of the runner, no subscription used
scripts/eval-report.sh ~/.zenbot/evals/<run>      # print a run's report again
```

Every run starts from a fresh copy of the task's files, on its own kernel and database, so the live
service and its data are never touched. Prompts go through `zen ask`, so the whole harness is
measured: system prompt, history, tools, engine. Results are in `~/.zenbot/evals/<run>/`.

The report compares, per task and in total: checks passed, cost, tokens, cache hit rate, time and
tool errors. Tasks that passed less often or got over 20% slower or more expensive come first.
It also lists the engine versions, and warns when they differ between the two sides, because then
the comparison isn't clean.

## A task

`tasks/<name>/files/` is the workspace the run starts in. `tasks/<name>/task.json`:

```json
{
  "description": "What it tests.",
  "work": "build",
  "steps": [
    { "prompt": "What the owner would type." },
    { "shell": "rm config.toml" },
    { "prompt": "A follow-up in the same session." }
  ],
  "checks": [
    { "name": "tests pass", "run": "cargo test --offline -q" },
    { "name": "names the cause", "answer_contains": "split_whitespace" }
  ]
}
```

- `steps` run in order in one session. A `shell` step runs in the workspace between turns (to set up
  or change files the way the world would).
- `checks` run after the last step. `run` is a bash command in the workspace that must exit 0
  (`$TASK_DIR` is the task's directory, for comparing with the original files). `answer_contains`
  looks for text in the last answer, ignoring case.
- `work` is the kind of work (understand, shape, bet, build, verify, maintain, reflect, reach).
- `env` sets kernel settings for the task's runs on both sides (e.g. `{"ZEN_CONTEXT_TOKENS": "12000"}`
  to make summaries happen in a short task); a build that doesn't know a setting ignores it.
- `"selftest": true` keeps a task out of normal runs; it runs only when named with `--tasks`.

A good task is something the owner really asks for, with checks that can't pass without the work
being done. Before adding one, make sure its checks fail on the untouched files and pass on a
correct solution.
