# zenbot: local development loop

How to build, test, verify and ship a change to zenbot from this checkout. Written for a coding agent (or a person) starting a fresh session. `AGENTS.md` has the collaboration rules; this file has the how-to.

> This checkout is used to develop zenbot. Follow the checks below before shipping changes; install changes only through `scripts/upgrade.sh`.

**The rule this enables:** a change is built, unit-tested, linted and run through the end-to-end scenarios locally, exactly as CI runs them, before it goes into a pull request. It is installed only through `scripts/upgrade.sh`, which smoke-tests it and rolls back on failure.

---

## What runs where

The processes are listed in `AGENTS.md` and, file by file, in `MAP.md`. The kernel owns all state and runs every tool call; engines run with their own tools switched off. Keep it that way.

Config and state live in `~/.zenbot/`; MAP.md ("`~/.zenbot/`") lists every file, who writes it and
who reads it. The secrets there are `env`, `token`, `matrix.env`
and `matrix/`: never print them. The dev kernel has its own home, `~/.zenbot-dev` (below). Claude
Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`.

## Prerequisites

| Tool | Install | Used for |
|---|---|---|
| Rust (stable) with clippy | `rustup` (`ensure_rust` in `scripts/lib.sh` installs a minimal toolchain; add clippy with `rustup component add clippy`) | building and linting |
| `build-essential`, `pkg-config` | apt (`ensure_rust` installs them with Rust) | compiling |
| Docker with `docker compose` | `install.sh` | Postgres and SearXNG (`deploy/compose.yaml`) |
| `git`, `curl`, `jq` | apt | every script |
| `python3` | preinstalled on Ubuntu | the e2e test servers (`scripts/e2e/*.py`) |
| `pdftotext` (`poppler-utils`) | apt (`install.sh` installs it) | reading PDFs that `web_fetch` saves |
| `bubblewrap` | apt | the verifier's read-only shell and the sandbox for unapproved made tools; the e2e scenarios need it |
| Node.js 22+ | `install.sh` puts it in `~/.local/node` | installing the Codex CLI when absent (not needed at runtime) |
| `gh` | GitHub CLI | pull requests, checking CI |

`install.sh` sets up Docker, `git`, `curl`, `jq`, `bubblewrap`, `poppler-utils` and Node on a fresh VM, and Rust with the C toolchain only when it has to compile (it downloads prebuilt binaries when it can). Clippy and `gh` are yours to add. The scripts add `~/.local/node/bin` and `~/.cargo/bin` to `PATH` themselves.

Postgres must be running for e2e, dev and upgrade:

```bash
docker compose -f deploy/compose.yaml up -d --wait postgres
```

It listens on `127.0.0.1:5432`, user and password `zen`, live database `zen`. The database helpers run `psql`, `pg_dump` and `pg_restore` inside the container, so nothing else is needed on the host.

`web_search` without an API key uses SearXNG, the second compose service (settings in `deploy/searxng/settings.yml`). Start it for keyless web search in a dev or real kernel:

```bash
docker compose -f deploy/compose.yaml up -d searxng
curl -s 'http://127.0.0.1:8888/search?q=test&format=json' | jq '.results | length'
```

It listens on `127.0.0.1:8888` only (`ZEN_SEARXNG_URL` points elsewhere). The service starts it before `zend` (a failure there is ignored: `web_search` then returns an error naming the command above), and `apply-upgrade.sh` runs `docker compose … up -d` after a healthy upgrade. With `BRAVE_API_KEY` or `TAVILY_API_KEY` in `~/.zenbot/env`, that provider is used instead (`ZEN_SEARCH_PROVIDER` forces one) and SearXNG only rescues a failed call. The e2e scenarios and CI don't need it: they use a stub.

## The loop at a glance

```bash
# 1. branch
git switch -c fix/short-name

# 2. edit, then check exactly as CI does: build, tests, clippy, e2e, docs (AGENTS.md)
scripts/check.sh                                 # scripts/check.sh memory: only matching e2e scenarios

# 3. try it for real (faux model, own database, own port)
ZEN_FAUX=1 scripts/dev.sh                       # in a second terminal
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen ask --json -m faux/smoke "hi"

# 4. harness change? see which tasks it affects, run them, show the owner the report
scripts/eval.sh --plan
scripts/eval.sh

# 5. install on this box: smoke test, scheduled restart, auto rollback
scripts/upgrade.sh
tail -20 ~/.zenbot/upgrade.log

# 6. commit, push the branch, open a PR; merge only on green CI
```

## Build, test, lint (as CI runs them)

CI (`.github/workflows/ci.yml`, job `check`, on `ubuntu-22.04` with stable Rust) runs these on every pull request and every push to `main`:

```bash
docker compose -f deploy/compose.yaml up -d --wait postgres
cargo build --release --locked
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
ZEN_E2E_NO_BUILD=1 scripts/e2e.sh
```

Run the same locally before a pull request: `scripts/check.sh` does it in one command (build once, then the tests, clippy, the e2e scenarios on that build, and `scripts/check-docs.py --base origin/main`), prints how long each step took and stops at the first failure with a short summary. `scripts/check.sh <filter>` passes the filter to `scripts/e2e.sh`. `--locked` fails if `Cargo.lock` would change; don't add dependencies without a good reason.

**Limit build parallelism on this VM.** It has 2 CPUs and 7.7 GB of RAM, and parallel release builds have been OOM-killed. Set `CARGO_BUILD_JOBS=2` (e.g. `export CARGO_BUILD_JOBS=2` in your shell) and don't run two release builds at once (for example `cargo build` while `scripts/e2e.sh` or `scripts/upgrade.sh` is building).

UI changes (`crates/zen/src/tui/`, `editor.rs`) come with render or key tests: build an `App` at a fixed size with output captured (see tests beside the modules in `tui/`).

### Model workers and System One

Changes to the worker protocol must update `docs/worker-protocol.md` and every worker.
System One runs in `zend/src/score.rs` and calls OpenRouter directly; the `system-one` e2e
scenario uses a local HTTP stub to check its auth, model id and `bool` ↔ `noul` mapping.

## End-to-end scenarios

`scripts/e2e.sh` builds (unless `ZEN_E2E_NO_BUILD=1`), then runs each scenario on a kernel built from this checkout with the scripted faux model. It uses a throwaway database (`zen_e2e_<pid>`), throwaway git workspaces and a throwaway `HOME`, on port 18377 (`ZEN_E2E_PORT`). No subscription is used, nothing reaches the internet and the live service isn't touched. It needs Postgres running, plus `git`, `curl`, `jq`, `bubblewrap` and `python3`.

There are 23 scenarios (`run …` lines at the bottom of the script). Some start small test programs from `scripts/e2e/`:

| Program | Started by | What it is |
|---|---|---|
| `slow_worker.py` | `slow-summary` (`ZEN_WORKER_SLOW_CMD`) | a worker whose `complete` is deliberately slow (`ZEN_SLOW_SECS`) |
| `mcp_server.py` | `mcp` | an MCP server with `echo` and `add`: over stdio, and with `--http PORT` as streamable HTTP on the e2e port + 1 |
| `searxng_stub.py` | `web`, `wiki` (`ZEN_SEARXNG_URL`) | answers `/search?format=json` with fixed results, on the e2e port + 2 |
| `systemone_stub.py` | `system-one`, `scheduled-jobs` (`ZEN_S1_URL`) | an OpenRouter System One stand-in that checks auth and the `bool` → `noul` mapping, on the e2e port + 3 |
| `ws_prompt.py` | `names-suggestions` | a client, not a server: sends one message on a session's WebSocket as the terminal app does and prints the events that follow |

```bash
scripts/e2e.sh                # build, then every scenario
scripts/e2e.sh memory         # only scenarios whose name contains "memory"
ZEN_E2E_KEEP=1 scripts/e2e.sh # keep the database and files afterwards (it prints where)
```

It prints `ok` / `FAIL` per check, the kernel log path for a failed scenario, and exits non-zero if any check failed. After all scenarios it checks that every session's tape is numbered without gaps and its hash chain recomputes.

### Adding a scenario

Add one when you change the kernel's behavior.

1. If the turn needs a scripted model, add a faux script in `scripts/e2e/<name>.json` (format below). Placeholders like `ROUTE` or `TARGET` are filled by `script <file> 's/ROUTE/bounded/' …`.
2. Write a function in the scenarios section of `scripts/e2e.sh`:
   - `ws=$(new_workspace <name>)` makes a git workspace with one commit.
   - `start_kernel "$ws" "$(script <file>.json)" [ENV=VALUE …]` starts the kernel with `ZEN_FAUX=1`. Pass `""` for the default faux turn (one `bash` call, then an answer).
   - `zen ask --json -m faux/smoke "…"` runs a turn; `zen ask --json -s "$sid" "…"` continues a session.
   - `q "SQL"` queries the scenario's database.
   - `check "description" <command…>` records a result; `eq actual expected` compares strings.
3. Register it at the bottom with `run <name> <function>`.

## Testing without a subscription: the faux model

With `ZEN_FAUX=1`, `zen-engine` also lists `faux/smoke`, a scripted model that drives a real turn through the kernel. By default it makes one `bash` call, then answers "Smoke test passed: …".

`ZEN_FAUX_SCRIPT` points to a JSON file of steps, or an object of step lists keyed by kind of session (`verify` for a verifier the `verify` tool starts, `default` otherwise). Steps:

```json
{"tool": "bash", "args": {"command": "echo hi"}}
{"text": "an answer"}
{"sleep": 5}
{"exit": 1}
```

A step can also carry `"when": "text"` (run only if the prompt contains the text, so one script serves several prompts, as `scripts/e2e/reach.json` does for `mcp` and `web`) or `"ignore_abort": true`. Use them to test tools, abort, the watchdog and crash recovery. See `docs/worker-protocol.md` ("Testing without a model") and the scripts in `scripts/e2e/`.

## The dev kernel

`scripts/dev.sh` runs a kernel from this checkout in the foreground, next to the installed service:

```bash
ZEN_FAUX=1 scripts/dev.sh
```

It:

- starts Postgres (`docker compose … up -d --wait postgres`);
- creates the database `zen_dev` if missing (override with `ZEN_DEV_DB`);
- loads `~/.zenbot/env` (workers, models, budgets) and uses the token in `~/.zenbot/token`;
- runs `cargo build --release -q`, then `exec`s `./target/release/zend` on port **18100** (override with `ZEN_DEV_PORT`);
- sets `ZEN_HARNESS` to this checkout's commit, so its turns record this build;
- uses its own zenbot home, `~/.zenbot-dev` (`ZEN_DEV_HOME`), for prompt files, skills, `mcp.json` and `MEMORY.md`: it starts with the defaults, and the dev database's memory never overwrites the live `~/.zenbot/global/MEMORY.md`. Copy your prompt files (or an `mcp.json`) there to try them.

Every non-live kernel gets its own `ZEN_HOME`: the dev kernel `~/.zenbot-dev`, the upgrade smoke kernel `<smoke workspace>/.zenbot`, eval kernels `<task workspace>.zenbot` (so evals run on the default prompt files and skills, not the owner's), and e2e kernels a throwaway `HOME`. Cut tool output, web PDFs and the wiki follow `ZEN_HOME` too (`<zen home>/outputs`, `<zen home>/global/wiki`).

`web_search` in the dev kernel needs SearXNG running (above) or a search key in `~/.zenbot/env`; `web_fetch` reaches only public addresses, so it can't fetch the dev kernel or anything else on this VM.

Local endpoints:

| Service | Address |
|---|---|
| Installed service | `http://127.0.0.1:8100` (or `ZEN_PORT` in `~/.zenbot/env`) |
| Dev kernel (`scripts/dev.sh`) | `http://127.0.0.1:18100`, database `zen_dev` |
| Upgrade smoke kernel (`upgrade.sh`) | `http://127.0.0.1:18199` (`ZEN_SMOKE_PORT`), on a throwaway copy of the live database |
| e2e kernel | `http://127.0.0.1:18377` (`ZEN_E2E_PORT`), database `zen_e2e_<pid>` |
| Eval kernels | `http://127.0.0.1:18301` (`ZEN_EVAL_PORT`) and the next ports, one per run at once (`--jobs`); one database `zen_eval_<run>_<n>` per run |
| Postgres | `127.0.0.1:5432`, user/password `zen` |
| SearXNG | `http://127.0.0.1:8888` (`ZEN_SEARXNG_URL`) |
| Health | `GET /health` on any kernel: `ok`, `db`, `mind`, `workers`, `busy`, `commit`, `version` |

Talk to the dev kernel with the CLI you just built:

```bash
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen ask --json -m faux/smoke "run the smoke command"
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen        # interactive
```

`ZEN_FAUX` is not set by `dev.sh`; pass it as above (or have it in `~/.zenbot/env`) to get `faux/smoke`. Without it, the dev kernel serves real models on the owner's subscriptions.

Caution: `dev.sh` does not set `ZEN_WORKSPACE`, so the dev kernel's tools work in `$HOME` (the kernel's default), on real files. Set `ZEN_WORKSPACE=/some/scratch/dir` when that matters.

## Testing for real against the running service

The installed service runs the installed binaries, not your checkout. So:

- **CLI changes** (`crates/zen`): use `./target/release/zen` against the service directly.
- **Kernel or worker changes**: use the dev kernel above, or install with `scripts/upgrade.sh` first.

```bash
./target/release/zen ask --json "…"                      # default model, real subscription
./target/release/zen ask --json -m faux/smoke "…"        # only if the service runs with ZEN_FAUX=1
```

`zen ask --json` prints one JSON object: `session_id`, `text`, `model`, `effort`, `tools` (each with `name` and `is_error`), `error`, `usage`, and the kernel's turn records (`turn`, `turns`). It exits non-zero on failure.

## Applying a change: `scripts/upgrade.sh`

This is the only way to install a change. **Never** run `systemctl restart zenbot` or kill `zend` yourself: that kills the session you are running in. `upgrade.sh` is safe to run from inside a zen session.

```bash
scripts/upgrade.sh           # build, check, smoke test, then schedule the install
scripts/upgrade.sh --check   # build, check and smoke test only; installs nothing
```

What it does, in order:

1. **Hooks.** Sets `git config core.hooksPath scripts/git-hooks`.
2. **Build.** Tries `scripts/fetch-release.sh`: if nothing under `crates/`, `Cargo.toml` or `Cargo.lock` differs from `HEAD` (and no untracked files under `crates/`), on x86_64 Linux, and CI published binaries for this commit, it downloads them (checksum verified) into `target/release`. Otherwise it runs `cargo build --release`, installing Rust first if missing. `ZEN_BUILD_FROM_SOURCE=1` forces a local build.
3. **Check.** `cargo test --release -q` (skipped for prebuilt binaries: CI already ran it). `zen-engine` must answer a JSON-RPC `ping`; `zen --version` must run. It does **not** run clippy or the e2e scenarios: run those yourself.
4. **Smoke.** Copies the live database into `zen_smoke_<pid>` and starts the new `zend` on port 18199 with `ZEN_FAUX=1`, the service's settings and its own `ZEN_HOME` in the smoke workspace. Any pending migrations are applied to that copy, not to the live database. It runs one scripted turn on `faux/smoke`, which must answer "Smoke test passed" with a successful tool call. The copy is dropped afterwards. It lists migrations pending on the live database.
5. **`--check` stops here** and prints `Check OK (<commit>[, uncommitted changes]); nothing installed.`
6. **Stage and schedule.** Copies the binaries it just tested, with their commit (and a note if the tree had uncommitted changes), into `~/.zenbot/upgrades/<id>/` (the id is a nanosecond timestamp; stages older than 2 hours are removed). Then it starts `scripts/apply-upgrade.sh <stage>` detached via `sudo systemd-run` and returns. The install comes from the stage, so checking out another commit or rebuilding `target/` (worktrees may share one) while it waits changes nothing.

`apply-upgrade.sh` then, on its own:

1. Waits until the service's `/health` says `"busy":0` (no session working). After 30 minutes it goes ahead anyway and logs that it did.
2. Takes a lock (`~/.zenbot/upgrades/.lock`), so two installs never interleave. The newest request wins: if a newer stage is still waiting, or a newer one was already installed (`~/.zenbot/upgrades/.installed`), it logs `upgrade to <commit> skipped` and exits.
3. If migrations are pending, backs up the live database to `~/.zenbot/backups/<UTC time>-<previous version>.dump` (last 10 kept). If the backup fails, it aborts and changes nothing.
4. Keeps the old binaries as `~/.zenbot/bin/<name>.prev`, installs `zend`, `zen` and `zen-engine` from the stage, writes the stage's commit to `~/.zenbot/version`, and runs `sudo systemctl restart zenbot`. The kernel applies migrations at start.
5. Waits up to 45 seconds for `/health` to say `"ok":true`. The stage is removed either way. If it is healthy, it removes the systemd timers older versions installed (`remove_old_timers`; the kernel schedules that work itself now, D-046) and runs `docker compose -f deploy/compose.yaml up -d`, so a new compose service (such as SearXNG) arrives with the upgrade; a failure there is logged, not rolled back. If it isn't healthy, it logs the last 30 service log lines, puts the `.prev` binaries and the old version back, and restarts again. **A rollback does not restore the database.** When there was a backup, the log prints the exact command to restore it.

The restart ends the current turn's connection. In a zen session, the owner reconnects by sending the next message.

Afterwards, always check the result:

```bash
tail -20 ~/.zenbot/upgrade.log    # "upgrade OK: now running <commit>[, uncommitted changes]", skipped, or FAILED / rolled back
cat ~/.zenbot/version
zen status
```

If it rolled back, read the log, fix, and run `scripts/upgrade.sh` again.

Note: `upgrade.sh` installs what is in your working tree, uncommitted changes included, but `~/.zenbot/version` records only the commit (the log adds ", uncommitted changes"). Commit first if you want the version to mean something.

To update from GitHub instead (on `main`, clean tree): `zen upgrade` (or `/upgrade`), which runs `scripts/self-update.sh`: `git pull --ff-only origin main`, then `upgrade.sh`. It refuses on another branch or with local changes.

## Database changes

- Add a new file in `crates/zend/migrations/` (`NNNN_name.sql`, next number). **Never** edit an applied migration. Migrations are compiled into `zend` and applied when it starts.
- **Expand-only.** Add tables, columns and indexes. Don't drop, rename or change the type of anything in the same release that stops using it; do that in a later release. A rollback swaps the binaries back but not the schema, so the previous build must keep working on the new schema.
- `upgrade.sh` tries new migrations on a copy of the live database first; `--check` stops after that test.

Helpers (`scripts/db.sh`; the live database is `DATABASE_URL` from the environment or `~/.zenbot/env`, else `zen`):

```bash
scripts/db.sh pending          # migrations in this checkout the live database hasn't applied
scripts/db.sh backup [label]   # dump the live database to ~/.zenbot/backups (keeps the last 10)
```

Restoring a backup stops the service, so the owner does it, outside any zen session:

```bash
sudo systemctl stop zenbot && docker compose -f deploy/compose.yaml exec -T postgres \
  pg_restore -U zen --clean --if-exists -d zen < ~/.zenbot/backups/<file>.dump && sudo systemctl start zenbot
```

## Harness evals

Changes to the harness (system prompt, history, tools, workers, model or effort handling) get an eval before they are merged. The eval is **advisory**: run it, show the owner the report (in the pull request too), and ask whether to merge. The owner decides. It is never an automatic gate.

```bash
scripts/eval.sh --plan                             # which tasks the change affects, and why; runs nothing
scripts/eval.sh                                    # those tasks: this checkout (new) vs the installed version (base)
scripts/eval.sh --full                             # every task: a large harness change, or when the owner asks
scripts/eval.sh --base main --tasks repo-question --repeat 3
scripts/eval.sh --model claude/claude-sonnet-5-5 --effort low
scripts/eval.sh --model faux/smoke --tasks smoke   # self-test of the runner, no subscription used
scripts/eval-report.sh ~/.zenbot/evals/<run>       # print a run's report again
```

Other options: `--jobs N` (runs at once, default 3 or `ZEN_EVAL_JOBS`), `--fresh` (run the base again instead of using the cache), `--native claude|codex` (the base is the vendor's own CLI), `--base-model`, `--base-effort`, `--only new|base`, `--keep`. Without `--model` it asks the running service (`/api/models`) for its default model, so the service must be up.

Which tasks run (D-050): the files that differ between the base and this checkout, uncommitted ones included, are mapped to areas by `evals/areas.txt`; the tasks whose `areas` meet them run, plus the `core` tasks. A change to `crates/zen-engine` or `crates/zen-proto` runs every task; a change outside the harness (docs, the `zen` client, scripts) needs no eval, and `eval.sh` says so and stops. The selection and its reasons are at the end of the report.

Each run (harness × task × repeat) gets a fresh copy of the task's files, its own kernel on its own port (18301 and up), its own database (`zen_eval_<run>_<n>`, dropped after it) and its own `ZEN_HOME` (default prompt files and skills, not the owner's), so runs at the same time can't see each other. Base and new runs of a task are interleaved. Stopping `eval.sh` (Ctrl-C) stops every kernel it started and drops their databases. The new side is this checkout, uncommitted changes included. The base build is cached in `~/.zenbot/evals/builds/`, and base results that completed in `~/.zenbot/evals/cache/` for `ZEN_EVAL_CACHE_DAYS` (7) days, keyed by base commit, model, effort, the task's files and the engine versions; the report lists the ones it reused. Results and `report.md` go to `~/.zenbot/evals/<run>/`. Real models use the owner's subscription, so `--jobs` also sets how many turns run on it at once.

Tasks live in `evals/tasks/<name>/` (`task.json` plus `files/` and, for hidden tests, `hidden/`). See `evals/README.md` for the format and what makes a good task.

## Scheduled jobs

The kernel runs a scheduler (`jobs.rs`, D-046; DESIGN.md "Scheduled jobs"). Its own jobs are seeded at start: `sleep` (03:00 UTC) and `engines` (04:00 UTC). Agent jobs run a prompt in a fresh session of kind `job` and record its report; the owner adds them with `zen jobs add`, the agent with the `schedule` tool (live only past the System One gate, else paused until `zen jobs resume`).

```bash
zen jobs                                      # every job: schedule, next run, last result
zen jobs runs [name]                          # recent runs and their reports
zen jobs run sleep                            # run a job now (here: the memory sleep)
zen jobs add morning-brief -s "0 7 * * 1-5" -p "…"   # an agent job (America/Sao_Paulo unless --tz)
zen jobs pause|resume|rm <name>
```

Throwaway kernels must not run the copy's jobs (an engine update on the real CLIs, agent jobs on real models): `upgrade.sh`'s smoke kernel, the e2e and eval kernels set `ZEN_JOBS=0`, and so does `dev.sh` unless you set `ZEN_JOBS=1`. The `scheduled-jobs` e2e scenario turns it on with `ZEN_JOBS_TICK=0.5` so runs start within a second.

## Memory sleep

The kernel's `sleep` job tidies short-term memory nightly (03:00 UTC); `zen memory sleep` runs it now.

```bash
zen memory                                   # short-term memory and the last sleep
zen memory --tier archived                   # what the sleeps archived
zen jobs runs sleep                          # when it ran and what it did
```

The sleep promotes on its own (D-045): lasting entries about the owner move to `USER.md`, lasting guidance to `IDENTITY.md` (under `## Learned`; only `owner`/`verified` entries), lasting knowledge is copied into the wiki. `ZEN_PROMOTE_BAR` (0.9) sets the bar. A prompt file over its cap (`ZEN_USER_CHARS`, `ZEN_IDENTITY_CHARS`) is compacted by a session the sleep starts (`sleep: compact USER.md` in `zen sessions`). Every change to a prompt file, by the sleep or the agent, first saves the old version to `~/.zenbot/backups/prompt-files/<name>-<time>.md` (kept 90 days); to undo, copy it back.

## Search index

The kernel indexes every session's turns, every short-term memory (and old `long` rows) and every wiki page into `search_docs` in the background (`search::index_loop`, every `ZEN_INDEX_SECS`, default 20 s), for the `search` tool. Exact-name and full-text search need nothing else. Vector search needs embeddings, which the indexer fetches only when `OPENROUTER_API_KEY` is set and `ZEN_S1_PRIVATE` isn't `0` (`ZEN_EMBED_MODEL`, `ZEN_EMBED_URL`; `ZEN_EMBED=0` turns them off). That sends turn, memory and wiki text to the provider, so leave the key out of a dev kernel's environment unless you mean to test embeddings: `dev.sh` loads `~/.zenbot/env`, and the e2e kernels unset `OPENROUTER_API_KEY`. Migration 0017 needs the `vector` and `pg_trgm` extensions, which the `pgvector/pgvector:pg16` image has.

```bash
docker compose -f deploy/compose.yaml exec -T postgres psql -U zen -d zen_dev \
  -c "SELECT kind, count(*), count(embedding) FROM search_docs GROUP BY kind"   # what the dev kernel indexed
```


## Wiki

The `capture` tool writes the wiki: markdown pages in `ZEN_WIKI_DIR` (default `<zen home>/global/wiki`, so `~/.zenbot-dev/global/wiki` for the dev kernel), a git repository the kernel creates and commits to (`index.md` lists the pages, `log.md` records each capture). The agent edits page summaries itself; those edits are committed by the next capture or the nightly sleep, which also lists wiki problems (pages with no summary, links to missing pages) in its note. Pages are indexed for `search` (scope `wiki`) by file modification time.

```bash
git -C ~/.zenbot-dev/global/wiki log --oneline | head   # what the dev kernel captured
cat ~/.zenbot-dev/global/wiki/index.md
```

## Workshop: skills and tools the agent makes

`save_skill` creates or improves a skill (new ones are drafts in `global/skills/_proposed/`); `save_tool` makes a tool in `global/tools/<name>/`, offered through `find_tools` / `call_tool` as `made_<name>`. The owner reviews them from the CLI:

```bash
zen skills                                   # skills with their use, drafts marked with their fingerprint; the made tools with theirs
zen skills accept work/release-notes 3f9a0c1d2e4b  # activate the draft you reviewed (reject moves it to _archived)
zen tools accept word-count 81c2d0a7f3e9     # let the made tool you reviewed run unsandboxed, with the network
git -C ~/.zenbot-dev/global/skills log --oneline   # every change the dev kernel made to its skills
```

A draft also becomes active when the owner accepts a session that loaded it (`zen sessions decide <id> accept`), unless it opens a new domain. Accepting or rejecting names the fingerprint `zen skills` showed and is refused when the content changed since. Each run of a made tool uses a temporary copy of its folder (what it writes there is dropped). Unapproved tools run in bubblewrap with the filesystem read-only and no network; approval is stored in the `made_tools` table. The nightly sleep flags skills unused for 30 days and archives them at 90. To test without a subscription, see the `workshop` e2e scenario and `scripts/e2e/workshop.json`.

## Delegation and the routing policy

`delegate` runs subtasks as subagent sessions (`kind = 'subagent'`, hidden from `zen sessions`), several at once with `tasks`. Unless a task names a model, the routing policy picks one per kind of work; the kind comes from System One, so without `ZEN_S1_MODEL` every task is `unknown` and uses the `default` route (else the parent's model).

```bash
zen policy                                        # routes, evidence per kind and model, suggestions
zen policy set default faux/smoke                 # route a kind of work (here every unknown one)
zen policy set build claude/claude-opus-5-5 --candidates codex/gpt-6.1-sol --explore 0.1
zen policy undo                                   # a new version with the previous data
```

Every choice is a `decisions` row (`point = 'model'`); the owner's verdict on the parent session is the evidence. The nightly sleep switches a route only on clear evidence (`ZEN_POLICY_MIN_JUDGED` judged subtasks per model, default 20), as a new policy version. The `delegation` e2e scenario (`scripts/e2e/delegate.json`) shows it without a subscription.

## Engine updates

The Claude Code and Codex CLIs track their latest versions. The kernel's `engines` job runs `scripts/update-engines.sh` daily (04:00 UTC), with the kernel's secret variables removed from its environment. Each update downloads from the vendor, checks the SHA-256, runs one real tool-free completion through `zen-engine`, and rolls back if it fails. It never commits, builds or restarts zenbot.

```bash
scripts/update-engines.sh --check            # installed vs latest; changes nothing
scripts/update-engines.sh                    # update now (safe from inside a zen session)
ZEN_ENGINES=claude scripts/update-engines.sh # only some engines
ZEN_ENGINES_FAIL=codex scripts/update-engines.sh  # force a failed check, to test the rollback
zen jobs runs engines                        # when it ran, and its output
```

Results go to `~/.zenbot/upgrade.log` (`engines:` lines) and `~/.zenbot/engines.json` (shown by `zen status`).


Code that depends on a CLI's flags or output should fail loudly, so the post-update check catches a change.

## CI and releases

`.github/workflows/ci.yml` has four jobs (there is no separate release workflow):

- **`check`**: on every pull request and every push to `main`. Build, unit tests, clippy, e2e (commands above). A newer push to a pull request cancels its running check.
- **`matrix`**: on the same events as `check`, beside it. Build, unit tests and clippy of `crates/zen-matrix` (its own workspace); not in the release tarball, and `release` doesn't wait for it.
- **`docs`**: on the same events. `scripts/check-docs.py` (with `--base` on a pull request): the lists the docs keep (routes, tools, settings, tables, events, tape kinds, slash commands, e2e scenarios, crates) match the code, the paths, settings and decisions the docs name exist, and a change to the code adds a PROGRESS.md entry. AGENTS.md, "Keeping the docs true".
- **`release`**: only on a push to `main` that passed `check`. Builds the `dist` profile (fat LTO), checks `zen-engine` answers `ping` and `zen --version` runs, and uploads `zenbot-x86_64-linux-<sha12>.tar.gz` plus `.sha256` to the rolling `edge` prerelease (newest 20 builds kept).

So binaries exist only for commits on `main` that passed. `install.sh`, `upgrade.sh` and `eval.sh` use them via `scripts/fetch-release.sh`; when there are none (yet), they compile locally. Right after a merge, CI needs a few minutes.

```bash
scripts/fetch-release.sh --check origin/main && echo "binaries published"
gh run list --limit 5
```

## The Matrix channel

`crates/zen-matrix` is its own Cargo workspace (D-049), so the commands above don't build it. CI's
`matrix` job runs the same three in its directory:

```bash
cd crates/zen-matrix && cargo build --release --locked && cargo test --release --locked && cargo clippy --release --locked --all-targets -- -D warnings
scripts/matrix-e2e.sh      # end to end, through E2EE, on a throwaway homeserver and a faux kernel (Docker)
scripts/matrix.sh          # build, test, install, sign in if needed, restart the zen-matrix service (--build: build and test only)
```

The first build compiles matrix-sdk (about 15 minutes on 2 cores; relinking takes a few minutes).
The e2e test needs nothing paid: the kernel runs only the faux model with OpenRouter keys unset.

## Git flow

1. One branch per feature or fix, off `main`.
2. One concern per commit. The subject says what changed; the body says why, and what was tested.
3. Commits made from a zen session get `Zen-Session` and `Co-Authored-By` trailers from `scripts/git-hooks/prepare-commit-msg`. Don't remove them.
4. Push the branch and open a pull request (`gh pr create`). Merge only when CI is green; harness changes also need the owner's OK on an eval.
5. Update the docs in the same pull request (the table in `AGENTS.md`). Design decisions change only with the owner's OK.
6. After a merge: `git checkout main && git pull`.

## Hard rules

- **Never** run `systemctl restart zenbot` (or stop/start) or kill `zend` from inside a session. Use `scripts/upgrade.sh`.
- Migrations are **expand-only**, in new files; never edit an applied one.
- No secrets in the repo, logs or tool output. **Never print `~/.zenbot/token`** or `~/.zenbot/env` (it holds the token and API keys, such as search keys). Put secrets an MCP server needs in `env` and refer to them as `${VAR}` in `mcp.json`, listed in that server's `allow_env`.
- Keep engines' own tools switched off; every action goes through the kernel.
- Don't add dependencies without a good reason.

## Troubleshooting

```bash
zen status                          # kernel, database, workers, sign-in, engine versions
curl -s http://127.0.0.1:8100/health   # ok / db / mind / workers / busy
journalctl -u zenbot -n 50          # service logs
tail -50 ~/.zenbot/upgrade.log      # upgrades, rollbacks, engine updates
scripts/db.sh pending               # migrations not yet applied
```

| Symptom | Look at |
|---|---|
| `upgrade.sh` says `BUILD FAILED` | the compiler errors it printed; fix and re-run |
| `CHECK FAILED: zen-engine did not answer ping` | run the ping by hand: `echo '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}' \| ./target/release/zen-engine` |
| `SMOKE TEST FAILED` | the last 20 kernel log lines it printed; often a migration that fails on the copy of the live database |
| `could not copy the live database` | Postgres not running: `docker compose -f deploy/compose.yaml up -d --wait postgres` |
| Upgrade scheduled but nothing changed | `~/.zenbot/upgrade.log`: it waits for `"busy":0`, up to 30 minutes |
| `upgrade FAILED health check; rolling back` | the service logs copied into `upgrade.log`; fix and re-run. If migrations ran, the log has the restore command |
| `ROLLBACK ALSO UNHEALTHY` | `journalctl -u zenbot -n 50`; tell the owner |
| e2e `kernel did not start` | the kernel log path it printed; check the port isn't taken and Postgres is up |
| e2e check fails on the read-only shell | `bubblewrap` missing |
| e2e `mcp`, `web` or `wiki` fails to start its server | `python3` missing, or the e2e port + 1 / + 2 is taken |
| `web_search failed: … searxng at http://127.0.0.1:8888` | SearXNG not running: `docker compose -f deploy/compose.yaml up -d searxng` |
| `find_tools` lists problems | `~/.zenbot/mcp.json` (invalid JSON, a server with both or neither of `command`/`url`, an unset `${VAR}`, or a secret-named one the server doesn't list in `allow_env`); `GET /api/mcp` shows the same |
| `search` finds nothing recent | the indexer runs every `ZEN_INDEX_SECS`; `search` also indexes before it queries. Warnings `search index:` / `embedding search documents:` in the kernel log |
| A build is killed with no compiler error | out of memory: `CARGO_BUILD_JOBS=2`, one build at a time |
| Upgrade compiles instead of downloading | local changes under `crates/` or `Cargo.*`, or CI hasn't published this commit yet (`scripts/fetch-release.sh --check HEAD`) |
| `zen` says `no token` | `~/.zenbot/token` missing; `install.sh` creates it |
| Engine shows `rolled back` or `skipped` in `zen status` | `engines:` lines in `upgrade.log`; `skipped: check fails before updating` usually means signed out (`zen login`, needs the owner) |
