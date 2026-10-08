# zenbot: local development loop

How to build, test, verify and ship a change to zenbot from this checkout. Written for a coding agent (or a person) starting a fresh session. `AGENTS.md` has the collaboration rules; this file has the how-to.

> This checkout is used to develop zenbot. Follow the checks below before shipping changes; install changes only through `scripts/upgrade.sh`.

**The rule this enables:** a change is built, unit-tested, linted and run through the end-to-end scenarios locally, exactly as CI runs them, before it goes into a pull request. It is installed only through `scripts/upgrade.sh`, which smoke-tests it and rolls back on failure.

---

## What runs where

The processes are listed in `AGENTS.md` and, file by file, in `MAP.md`. The kernel owns all state and runs every tool call; engines run with their own tools switched off. Keep it that way.

Config lives in `~/.zenbot/`:

| File | What |
|---|---|
| `env` | the service's environment (`ZEN_PORT`, `ZEN_WORKERS`, `ZEN_TOKEN`, …); the dev scripts read it too |
| `token` | API token; the `zen` CLI reads it when `ZEN_TOKEN` is unset |
| `auth.json` | Legacy Pi sign-in (unused). **Secret: never print it.** |
| `version` | the commit the service runs |
| `upgrade.log` | results of upgrades and engine updates |
| `engines.json` | engine versions from the last update check |
| `backups/` | database dumps taken before migrations (last 10) |
| `evals/` | eval runs and cached base builds |
| `history` | prompt history |
| `AGENTS.md`, `USER.md` | system-wide prompt files: zenbot's environment, the owner (defaults written when missing, never overwritten) |
| `agents/zenbot/SOUL.md` | the agent's own prompt file: who zenbot is (one agent today; D-040) |
| `agents/zenbot/IDENTITY.md` | the agent's character and how it works, edited by approved proposal (D-045) |
| `global/MEMORY.md` | a copy of short-term memory, for reading |
| `global/skills/` | skills, `<domain>/<name>/SKILL.md`; drafts in `_proposed/`, retired ones in `_archived/`; a git repository once `save_skill` first commits |
| `global/tools/` | tools the agent made (`save_tool`): `<name>/tool.json` and files; a git repository (`ZEN_TOOLS_DIR`) |
| `mcp.json` | the owner's MCP servers (`mcpServers`; `${VAR}` filled from `env`), reached through `find_tools` / `load_tool` / `call_tool` |
| `outputs/` | full text of cut tool output, PDFs saved by `web_fetch`, MCP output over 50 KB |
| `global/wiki/` | the wiki (`ZEN_WIKI_DIR`): markdown pages, `index.md`, `log.md`, its own git repository; written by the `capture` tool |
| `SOUL.md`, `MEMORY.md`, `wiki`, `skills`, `tools` | symlinks to the paths above, left by the move from the old flat layout so a rolled-back build still finds the files; removed in a later release |
| `dev/` | the dev kernel's own home (`scripts/dev.sh`) |

Claude Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`.

## Prerequisites

| Tool | Install | Used for |
|---|---|---|
| Rust (stable) with clippy | `rustup` (`ensure_rust` in `scripts/lib.sh` installs a minimal toolchain; add clippy with `rustup component add clippy`) | building and linting |
| `build-essential`, `pkg-config` | apt | compiling |
| Docker with `docker compose` | `install.sh` | Postgres and SearXNG (`deploy/compose.yaml`) |
| `git`, `curl`, `jq` | apt | every script |
| `python3` | preinstalled on Ubuntu | the e2e test servers (`scripts/e2e/*.py`) |
| `pdftotext` (`poppler-utils`) | apt (`install.sh` installs it) | reading PDFs that `web_fetch` saves |
| `bubblewrap` | apt | the verifier's read-only shell and the sandbox for unapproved made tools; the e2e scenarios need it |
| Node.js 22+ | `install.sh` puts it in `~/.local/node` | installing the Codex CLI when absent (not needed at runtime) |
| `gh` | GitHub CLI | pull requests, checking CI |

`install.sh` sets all of this up on a fresh VM except Rust and clippy (it downloads prebuilt binaries when it can). The scripts add `~/.local/node/bin` and `~/.cargo/bin` to `PATH` themselves.

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

# 2. edit, then check exactly as CI does
cargo build --release --locked
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
ZEN_E2E_NO_BUILD=1 scripts/e2e.sh

# 3. try it for real (faux model, own database, own port)
ZEN_FAUX=1 scripts/dev.sh                       # in a second terminal
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen ask --json -m faux/smoke "hi"

# 4. harness change? run an eval and show the owner the report
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

Run the same four locally before a pull request. `--locked` fails if `Cargo.lock` would change; don't add dependencies without a good reason.

**Limit build parallelism on this VM.** It has 2 CPUs and 7.7 GB of RAM, and parallel release builds have been OOM-killed. Set `CARGO_BUILD_JOBS=2` (e.g. `export CARGO_BUILD_JOBS=2` in your shell) and don't run two release builds at once (for example `cargo build` while `scripts/e2e.sh` or `scripts/upgrade.sh` is building).

UI changes (`crates/zen/src/tui/`, `editor.rs`) come with render or key tests: build an `App` at a fixed size with output captured (see tests beside the modules in `tui/`).

### Model workers and System One

Changes to the worker protocol must update `docs/worker-protocol.md` and every worker.
System One runs in `zend/src/score.rs` and calls OpenRouter directly; the `system-one` e2e
scenario uses a local HTTP stub to check its auth, model id and `bool` ↔ `noul` mapping.

## End-to-end scenarios

`scripts/e2e.sh` builds (unless `ZEN_E2E_NO_BUILD=1`), then runs each scenario on a kernel built from this checkout with the scripted faux model. It uses a throwaway database (`zen_e2e_<pid>`), throwaway git workspaces and a throwaway `HOME`, on port 18377 (`ZEN_E2E_PORT`). No subscription is used, nothing reaches the internet and the live service isn't touched. It needs Postgres running, plus `git`, `curl`, `jq`, `bubblewrap` and `python3`.

There are 21 scenarios (`run …` lines at the bottom of the script). Some start small test servers from `scripts/e2e/`:

| Server | Started by | What it is |
|---|---|---|
| `slow_worker.py` | `slow-summary` (`ZEN_WORKER_SLOW_CMD`) | a worker whose `complete` is deliberately slow (`ZEN_SLOW_SECS`) |
| `mcp_server.py` | `mcp` | an MCP server with `echo` and `add`: over stdio, and with `--http PORT` as streamable HTTP on the e2e port + 1 |
| `searxng_stub.py` | `web`, `wiki` (`ZEN_SEARXNG_URL`) | answers `/search?format=json` with fixed results, on the e2e port + 2 |

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
| Eval kernels | `http://127.0.0.1:18301` (`ZEN_EVAL_PORT`), databases `zen_eval_*` |
| Postgres | `127.0.0.1:5432`, user/password `zen` |
| SearXNG | `http://127.0.0.1:8888` (`ZEN_SEARXNG_URL`) |
| Health | `GET /health` on any kernel: `ok`, `db`, `mind`, `workers`, `busy` |

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

`zen ask --json` prints one JSON object: `session_id`, `text`, `tools` (each with `name` and `is_error`), `error`, usage. It exits non-zero on failure.

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
6. **Schedule.** Starts `scripts/apply-upgrade.sh` detached via `sudo systemd-run` and returns.

`apply-upgrade.sh` then, on its own:

1. Waits until the service's `/health` says `"busy":0` (no session working). After 30 minutes it goes ahead anyway and logs that it did.
2. If migrations are pending, backs up the live database to `~/.zenbot/backups/<UTC time>-<previous version>.dump` (last 10 kept). If the backup fails, it aborts and changes nothing.
3. Keeps the old binaries as `~/.zenbot/bin/<name>.prev`, installs `zend`, `zen` and `zen-engine` from `target/release`, writes the commit to `~/.zenbot/version`, and runs `sudo systemctl restart zenbot`. The kernel applies migrations at start.
4. Waits up to 45 seconds for `/health` to say `"ok":true`. If it is healthy, it refreshes the timers from `deploy/` (`install_timers`) and runs `docker compose -f deploy/compose.yaml up -d`, so a new timer or compose service (such as SearXNG) arrives with the upgrade; a failure there is logged, not rolled back. If it isn't healthy, it logs the last 30 service log lines, puts the `.prev` binaries and the old version back, and restarts again. **A rollback does not restore the database.** When there was a backup, the log prints the exact command to restore it.

The restart ends the current turn's connection. In a zen session, the owner reconnects by sending the next message.

Afterwards, always check the result:

```bash
tail -20 ~/.zenbot/upgrade.log    # "upgrade OK: now running <commit>", or FAILED / rolled back
cat ~/.zenbot/version
zen status
```

If it rolled back, read the log, fix, and run `scripts/upgrade.sh` again.

Note: `upgrade.sh` installs what is in your working tree, uncommitted changes included, but `~/.zenbot/version` records only `HEAD`. Commit first if you want the version to mean something.

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
scripts/eval.sh                                    # this checkout (new) vs the installed version (base)
scripts/eval.sh --base main --tasks repo-question --repeat 3
scripts/eval.sh --model claude/claude-sonnet-5-5 --effort low
scripts/eval.sh --model faux/smoke --tasks smoke   # self-test of the runner, no subscription used
scripts/eval-report.sh ~/.zenbot/evals/<run>       # print a run's report again
```

Other options: `--base-model`, `--base-effort`, `--only new|base`, `--keep`. Without `--model` it asks the running service (`/api/models`) for its default model, so the service must be up.

Each run gets a fresh copy of the task's files, its own kernel on port 18301, its own database (`zen_eval_*`) and its own `ZEN_HOME` (default prompt files and skills, not the owner's). The new side is this checkout, uncommitted changes included. The base side is cached in `~/.zenbot/evals/builds/`. Results and `report.md` go to `~/.zenbot/evals/<run>/`. Real models use the owner's subscription.

Tasks live in `evals/tasks/<name>/` (`task.json` plus `files/`). See `evals/README.md` for the format and what makes a good task.

## Memory sleep

`zen-sleep.timer` runs `scripts/sleep.sh` nightly (03:00 UTC, up to 30 minutes' random delay): it asks the running kernel to tidy short-term memory (`POST /api/memory/sleep?trigger=nightly`). The kernel does the work; the script only waits for it to be healthy and prints the counts.

```bash
scripts/sleep.sh                             # sleep now (same as `zen memory sleep`)
zen memory                                   # short-term memory and the last sleep
zen memory --tier archived                   # what the sleeps archived
systemctl list-timers zen-sleep.timer        # next run
```

The sleep promotes on its own (D-045): lasting entries about the owner move to `USER.md`, lasting guidance to `IDENTITY.md` (under `## Learned`; the file's previous version is saved to `~/.zenbot/backups/<name>-<time>.md`; only `owner`/`verified` entries), lasting knowledge is copied into the wiki. `ZEN_PROMOTE_BAR` (0.9) sets the bar. To undo a move, copy the backup back.

## Search index

The kernel indexes every session's turns, every short-term memory (and old `long` rows) and every wiki page into `search_docs` in the background (`search::index_loop`, every `ZEN_INDEX_SECS`, default 20 s), for the `search` tool. Exact-name and full-text search need nothing else. Vector search needs embeddings, which the indexer fetches only when `OPENROUTER_API_KEY` is set and `ZEN_S1_PRIVATE` isn't `0` (`ZEN_EMBED_MODEL`, `ZEN_EMBED_URL`; `ZEN_EMBED=0` turns them off). That sends turn, memory and wiki text to the provider, so leave the key out of a dev kernel's environment unless you mean to test embeddings: `dev.sh` loads `~/.zenbot/env`, and the e2e kernels unset `OPENROUTER_API_KEY`. Migration 0017 needs the `vector` and `pg_trgm` extensions, which the `pgvector/pgvector:pg16` image has.

```bash
docker compose -f deploy/compose.yaml exec -T postgres psql -U zen -d zen_dev \
  -c "SELECT kind, count(*), count(embedding) FROM search_docs GROUP BY kind"   # what the dev kernel indexed
```

`install.sh` installs the timers, and `apply-upgrade.sh` refreshes them after a healthy upgrade (`install_timers` in `scripts/lib.sh`), so a new timer arrives with an upgrade.

## Wiki

The `capture` tool writes the wiki: markdown pages in `ZEN_WIKI_DIR` (default `<zen home>/global/wiki`, so `~/.zenbot-dev/global/wiki` for the dev kernel), a git repository the kernel creates and commits to (`index.md` lists the pages, `log.md` records each capture). The agent edits page summaries itself; those edits are committed by the next capture or the nightly sleep, which also lists wiki problems (pages with no summary, links to missing pages) in its note. Pages are indexed for `search` (scope `wiki`) by file modification time.

```bash
git -C ~/.zenbot-dev/global/wiki log --oneline | head   # what the dev kernel captured
cat ~/.zenbot-dev/global/wiki/index.md
```

## Workshop: skills and tools the agent makes

`save_skill` creates or improves a skill (new ones are drafts in `skills/_proposed/`); `save_tool` makes a tool in `tools/<name>/`, offered through `find_tools` / `call_tool` as `made_<name>`. The owner reviews them from the CLI:

```bash
zen skills                                   # skills with their use, drafts marked; the made tools
zen skills accept work/release-notes         # activate a draft (reject moves it to _archived)
zen tools accept word-count                  # let a made tool run unsandboxed, with the network
git -C ~/.zenbot-dev/skills log --oneline    # every change the dev kernel made to its skills
```

A draft also becomes active when the owner accepts a session that loaded it (`zen sessions decide <id> accept`), unless it opens a new domain. Unapproved tools run in bubblewrap with the filesystem read-only and no network; approval is stored in the `made_tools` table. The nightly sleep flags skills unused for 30 days and archives them at 90. To test without a subscription, see the `workshop` e2e scenario and `scripts/e2e/workshop.json`.

## Delegation and the routing policy

`delegate` runs subtasks as subagent sessions (`kind = 'subagent'`, hidden from `zen sessions`), several at once with `tasks`. Unless a task names a model, the routing policy picks one per kind of work; the kind comes from System One, so without `ZEN_S1_MODEL` every task is `unknown` and uses the `default` route (else the parent's model).

```bash
zen policy                                        # routes, evidence per kind and model, suggestions
zen policy set default faux/smoke                 # route a kind of work (here every unknown one)
zen policy set build claude/claude-opus-5-5 --candidates codex/gpt-5.5 --explore 0.1
zen policy undo                                   # a new version with the previous data
```

Every choice is a `decisions` row (`point = 'model'`); the owner's verdict on the parent session is the evidence. The nightly sleep switches a route only on clear evidence (`ZEN_POLICY_MIN_JUDGED` judged subtasks per model, default 20), as a new policy version. The `delegation` e2e scenario (`scripts/e2e/delegate.json`) shows it without a subscription.

## Engine updates

The Claude Code and Codex CLIs track their latest versions. `zen-engines.timer` runs `scripts/update-engines.sh` daily (04:00 UTC, up to an hour's random delay). Each update downloads from the vendor, checks the SHA-256, runs one real tool-free completion through `zen-engine`, and rolls back if it fails. It never commits, builds or restarts zenbot.

```bash
scripts/update-engines.sh --check            # installed vs latest; changes nothing
scripts/update-engines.sh                    # update now (safe from inside a zen session)
ZEN_ENGINES=claude scripts/update-engines.sh # only some engines
ZEN_ENGINES_FAIL=codex scripts/update-engines.sh  # force a failed check, to test the rollback
systemctl list-timers zen-engines.timer      # next run
```

Results go to `~/.zenbot/upgrade.log` (`engines:` lines) and `~/.zenbot/engines.json` (shown by `zen status`).


Code that depends on a CLI's flags or output should fail loudly, so the post-update check catches a change.

## CI and releases

`.github/workflows/ci.yml` has two jobs (there is no separate release workflow):

- **`check`**: on every pull request and every push to `main`. Build, unit tests, clippy, e2e (commands above). A newer push to a pull request cancels its running check.
- **`release`**: only on a push to `main` that passed `check`. Builds the `dist` profile (fat LTO), checks `zen-engine` answers `ping` and `zen --version` runs, and uploads `zenbot-x86_64-linux-<sha12>.tar.gz` plus `.sha256` to the rolling `edge` prerelease (newest 20 builds kept).

So binaries exist only for commits on `main` that passed. `install.sh`, `upgrade.sh` and `eval.sh` use them via `scripts/fetch-release.sh`; when there are none (yet), they compile locally. Right after a merge, CI needs a few minutes.

```bash
scripts/fetch-release.sh --check origin/main && echo "binaries published"
gh run list --limit 5
```

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
- No secrets in the repo, logs or tool output. **Never print `~/.zenbot/auth.json`**, `~/.zenbot/token` or `~/.zenbot/env` (it holds the token and API keys, such as search keys). Put secrets an MCP server needs in `env` and refer to them as `${VAR}` in `mcp.json`.
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
| `find_tools` lists problems | `~/.zenbot/mcp.json` (invalid JSON, a server with both or neither of `command`/`url`, an unset `${VAR}`); `GET /api/mcp` shows the same |
| `search` finds nothing recent | the indexer runs every `ZEN_INDEX_SECS`; `search` also indexes before it queries. Warnings `search index:` / `embedding search documents:` in the kernel log |
| A build is killed with no compiler error | out of memory: `CARGO_BUILD_JOBS=2`, one build at a time |
| Upgrade compiles instead of downloading | local changes under `crates/` or `Cargo.*`, or CI hasn't published this commit yet (`scripts/fetch-release.sh --check HEAD`) |
| `zen` says `no token` | `~/.zenbot/token` missing; `install.sh` creates it |
| Engine shows `rolled back` or `skipped` in `zen status` | `engines:` lines in `upgrade.log`; `skipped: check fails before updating` usually means signed out (`zen login`, needs the owner) |
