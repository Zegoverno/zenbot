# Evals

Fixed tasks for comparing two versions of zenbot's harness with the same model and thinking
level. Run them before a harness change is merged, and show the report to the owner, who decides. They
never block a merge or an upgrade on their own.

```bash
scripts/eval.sh --plan                            # which tasks this change affects, and why
scripts/eval.sh                                   # those tasks: this checkout vs the installed version
scripts/eval.sh --full                            # every task
scripts/eval.sh --base main --tasks repo-question --repeat 3
scripts/eval.sh --model claude/claude-sonnet-5-5 --effort low
scripts/eval.sh --model faux/smoke --tasks smoke  # self-test of the runner, no subscription used
scripts/eval.sh --native claude --model claude/claude-opus-5-5 --tasks coding-rust-forth
scripts/eval-report.sh ~/.zenbot/evals/<run>      # print a run's report again
scripts/eval-paired.py ~/.zenbot/evals/<run> [<run>…]  # paired wins, losses and cost ratios
```

`scripts/eval.sh --help` lists every option (`--jobs`, `--fresh`, `--base-model`, `--base-effort`,
`--only new|base`, `--keep`). The installed version is the commit in `~/.zenbot/version`; a base
build is cached in `~/.zenbot/evals/builds/<commit>`, from CI's release binaries when there are
some. `--native claude|codex` makes the base the vendor's own CLI with its own tools
(`scripts/eval-native.sh`), to measure what zenbot's harness adds or costs.

Every run starts from a fresh copy of the task's files, on its own kernel, port, database and zenbot
home (the default prompt files and skills, not the owner's; scheduled jobs off), so the live service
and its data are never touched and runs at the same time can't see each other. Prompts go through
`zen ask`, so the whole harness is measured: system prompt, history, tools, engine. Results are in
`~/.zenbot/evals/<run>/`.

## Which tasks run, and how fast

By default only the tasks a change can affect (D-050). The files that differ between the base and
this checkout (uncommitted ones included) are looked up in [`areas.txt`](areas.txt): each line maps
a path prefix to areas (prompt, history, tools, memory, search, compaction, jobs, wiki, skills,
delegation, web, mcp, coding). A task runs when one of its `areas` is among them. Three cheap tasks
are `core` (`repo-question`, `clear-request`, `recall-across-turns`) and run for any harness change.
`crates/zen-engine` and `crates/zen-proto` map to `all`, every task including the `coding-*` ones;
a path no line names (docs, the `zen` client, zen-matrix, scripts, the tasks themselves) isn't
harness, and a change to only those needs no eval. `--plan` prints the selection with its reasons
and stops; the report ends with the same. `--full` runs every task, `--tasks` the ones named. Some
areas (jobs, wiki, skills, delegation, web, mcp) have no task of their own yet: a change there runs
the core tasks, so add a task when one of them changes in a way the owner would notice.

Runs go `--jobs` at a time (default 3, `ZEN_EVAL_JOBS`), the base and new runs of each task side by
side, so both sides meet the same load. A base run that completed (whether or not its checks passed)
is cached in `~/.zenbot/evals/cache/` and reused for `ZEN_EVAL_CACHE_DAYS` (7) days, as long as the
base commit, model, effort, the task's files (`task.json`, `files/`, `hidden/`), the `claude` and
`codex` versions and the repeat number are the same; the report lists which base results came from
the cache and when. `--fresh` runs the base again. The `coding-zen-*` tasks share one Cargo target
directory (`~/.zenbot/evals/cargo-target-zen`), so their builds wait for each other's lock; their
times are noisier when they run at once.

The report compares, per task and in total: checks passed, cost, tokens, cache hit rate, time, tool
errors and unexpected cache breaks (`history`, `miss`; docs/context.md). Tasks that passed less
often or got over 20% slower or more expensive come first. It also lists the engine versions, and
warns when an engine ran at different versions across the runs, because then the comparison isn't
clean.

## A task

`tasks/<name>/files/` (optional) is the workspace the run starts in. `tasks/<name>/hidden/`
(optional) holds what the agent mustn't see, such as the tests a check runs (`$TASK_DIR/hidden/…`).
Helpers shared by checks live in `lib/`. `tasks/<name>/task.json`:

```json
{
  "description": "What it tests.",
  "areas": ["tools"],
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
  (`$TASK_DIR` is the task's directory, for comparing with the original files; shell steps have it
  too; `$ANSWER` is the last answer and `$TURNS` a file with one JSON line per turn, its `tools`
  included, for checks on what the agent did). `answer_contains` looks for text in the last answer, ignoring case, and `answer_lacks`
  checks it isn't there; with `"step": n` they look at the answer to the n-th prompt instead.
- `areas` are the parts of the harness the task exercises, from the list in `areas.txt`; `core`
  makes it run for every harness change (keep core tasks few and cheap). A task without areas runs
  every time.
- `work` is the kind of work (understand, shape, bet, build, verify, maintain, reflect, reach), a
  label for reading results; the runner doesn't use it.
- `env` sets kernel settings for the task's runs on both sides (e.g. `{"ZEN_CONTEXT_TOKENS": "12000"}`
  to make summaries happen in a short task); a build that doesn't know a setting ignores it.
- `"selftest": true` keeps a task out of normal runs; it runs only when named with `--tasks`.

A good task is something the owner really asks for, with checks that can't pass without the work
being done. Before adding one, make sure its checks fail on the untouched files and pass on a
correct solution.
