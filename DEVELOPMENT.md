# zenbot: local development loop

How to build, test, verify and ship a change to zenbot from this checkout. Written for a coding agent (or a person) starting a fresh session. `AGENTS.md` has the collaboration rules; this file has the how-to.

> This checkout is used to develop zenbot. Follow the checks below before shipping changes; install changes only through `scripts/upgrade.sh`.

**The rule this enables:** a change is built, unit-tested, linted and run through the end-to-end scenarios locally, exactly as CI runs them, before it goes into a pull request. It is installed only through `scripts/upgrade.sh`, which smoke-tests it and rolls back on failure.

## What runs where

Processes and private paths: `AGENTS.md`; file/route/table/setting map: `MAP.md`. Dev uses `~/.zenbot-dev`.

## Prerequisites

| Tool | Install | Used for |
|---|---|---|
| Rust (stable) with clippy | `rustup` (`ensure_rust` in `scripts/lib.sh` installs a minimal toolchain; add clippy with `rustup component add clippy`) | building and linting |
| `build-essential`, `pkg-config` | apt (`ensure_rust` installs them with Rust) | compiling |
| SQLite | bundled into `zend`; Python standard library in scripts | kernel state and test databases |
| `git`, `curl`, `jq` | apt | every script |
| `python3` | preinstalled on Ubuntu | the e2e test servers (`scripts/e2e/*.py`) |
| `pdftotext` (`poppler-utils`) | apt (`install.sh` installs it) | reading PDFs that `web_fetch` saves |
| `bubblewrap` | apt | the verifier's read-only shell and a tainted session's offline shell; the e2e scenarios need it |
| Node.js 22+ | `install.sh` puts it in `~/.local/node` | installing and running the Codex CLI (an npm package: Node runs it on every Codex turn) |
| `gh` | GitHub CLI | pull requests, checking CI |

`install.sh` sets up `git`, `curl`, `jq`, `bubblewrap`, `poppler-utils` and Node on a fresh VM, and Rust with the C toolchain only when it has to compile (it downloads prebuilt binaries when it can). Clippy and `gh` are yours to add. The scripts add `~/.local/node/bin` and `~/.cargo/bin` to `PATH` themselves.

The kernel embeds SQLite, FTS5 and sqlite-vec. No database daemon or Docker is needed for
builds, development, e2e, evals or upgrades. `DATABASE_URL` must name SQLite; the default is
`sqlite://<ZEN_HOME>/zen.sqlite3`. Scripts use Python's standard SQLite module.

For `web_search`, configure an external `ZEN_SEARXNG_URL` or a Brave/Tavily key. No provider
is started automatically. The e2e scenarios and CI use local stubs. Matrix's optional encrypted
channel e2e may still run its throwaway homeserver in Docker.

## The loop at a glance

```bash
# 1. branch
git switch -c fix/short-name

# 2. edit, then check exactly as CI does: build, tests, clippy, e2e, docs (AGENTS.md)
scripts/check.sh memory                          # focused: only the e2e scenarios matching "memory"
scripts/check.sh                                 # everything, as CI does (before the pull request)

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

CI (`.github/workflows/ci.yml`, job `check`, on `ubuntu-22.04` with stable Rust) runs `scripts/check.sh` on every pull request and every push to `main`, using isolated SQLite files. Its steps, in order, stopping at the first failure:

| Step | Command |
|---|---|
| `docs` | `scripts/check-docs.py --base origin/<base branch>` (a second; first, so a missing PROGRESS.md entry fails fast) |
| `build` | `cargo build --release --locked` |
| `py-sqlite` | `ensure_test_sqlite` from `scripts/test-sqlite.sh`: pinned SQLite for Python test helpers when needed |
| `storage` | `python3 scripts/test_storage.py` |
| `pg-archive` | `python3 scripts/test_postgres_archive.py` |
| `cutover` | `python3 scripts/test_cutover.py` |
| `tests` | `cargo test --release --locked` |
| `clippy` | `cargo clippy --release --locked --all-targets -- -D warnings` |
| `sizes` | `scripts/size-budget.sh`: `zend`, `zen` and `zen-engine` within their size budgets (measured size + ~15%; raise one on purpose, saying why) |
| `e2e` | `ZEN_E2E_NO_BUILD=1 scripts/e2e.sh [filter]` |

It prints how long each step took (in CI also on the job's summary page). Locally, `scripts/check.sh <filter>` is the fast path: every step, but only the e2e scenarios matching the filter; run it without a filter, or let CI run the full suite, before merging. `--locked` fails if `Cargo.lock` would change; don't add dependencies without a good reason.

**Limit build parallelism on this VM.** It has 2 CPUs and 7.7 GB of RAM, and parallel release builds have been OOM-killed. Set `CARGO_BUILD_JOBS=2` (e.g. `export CARGO_BUILD_JOBS=2` in your shell) and don't run two release builds at once (for example `cargo build` while `scripts/e2e.sh` or `scripts/upgrade.sh` is building).


## End-to-end scenarios

`scripts/e2e.sh` builds (unless `ZEN_E2E_NO_BUILD=1`), then runs each scenario on a kernel built from this checkout (or the binaries in `ZEN_E2E_BIN`) with the scripted faux model. Each scenario gets its own throwaway database (`zen_e2e_<pid>_<n>`), git workspaces, `HOME` and ports (`ZEN_E2E_PORT` + 20 × n, default 18377, and the next few for its test servers), so `ZEN_E2E_JOBS` of them (default 4) run at once. No subscription is used, nothing reaches the internet and the live service isn't touched. It needs `git`, `curl`, `jq`, `bubblewrap` and `python3`.

The scenarios are the `run …` lines at the bottom of the script: a name, then the functions run as its parts (each on a fresh kernel); the comment above each function says what it checks. The test servers some parts start (`scripts/e2e/*.py`) are listed in MAP.md ("Scripts").

```bash
scripts/check.sh search       # the fast local path: build, tests, clippy, only the matching e2e scenarios, docs
scripts/e2e.sh search         # only scenarios whose name or part function contains "search"
scripts/e2e.sh                # build, then every scenario (CI runs them all on every pull request)
ZEN_E2E_JOBS=1 scripts/e2e.sh # one at a time, e.g. to rule out a timing problem
ZEN_E2E_KEEP=1 scripts/e2e.sh # keep the databases and files afterwards (it prints where)
```

Locally, run the scenarios your change touches (`scripts/check.sh <filter>`); CI runs the full suite. It prints `ok` / `FAIL` per check, the kernel log path for a failed scenario, the seconds each scenario took, and exits non-zero if any check failed. Each scenario ends by checking that every session's tape is numbered without gaps and its hash chain recomputes.

### Adding a scenario

Extend an existing scenario when you change the kernel's behavior; add a new one only for a new setup (there are 20; a check that needs no running kernel is a unit test instead).

1. If the turn needs a scripted model, add a faux script in `scripts/e2e/<name>.json` (format below). Placeholders like `ROUTE` or `TARGET` are filled by `script <file> 's/ROUTE/bounded/' …`.
2. Write a function in the scenarios section of `scripts/e2e.sh`:
   - `ws=$(new_workspace <name>)` makes a git workspace with one commit.
   - `start_kernel "$ws" "$(script <file>.json)" [ENV=VALUE …]` starts the kernel with `ZEN_FAUX=1`. Pass `""` for the default faux turn (one `bash` call, then an answer).
   - `zen ask --json -m faux/smoke "…"` runs a turn; `zen ask --json -s "$sid" "…"` continues a session.
   - `q "SQL"` queries the scenario's database.
   - `check "description" <command…>` records a result; `eq actual expected` compares strings.
   - A test server listens on `$PORT` + 1 to + 19, never the base port of another scenario.
3. Register it at the bottom with `run <name> <function…>` (or add the function as a part of a scenario with the same setup).

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

- uses a private SQLite database `zen_dev.sqlite3` (override with `ZEN_DEV_DB`);
- loads `~/.zenbot/env` (workers, models, budgets) and uses the token in `~/.zenbot/token`, but drops the paid keys (`OPENROUTER_API_KEY`, `ZEN_S1_MODEL`, `BRAVE_API_KEY`, `TAVILY_API_KEY`), the folder settings (`ZEN_WORKSPACE`, `ZEN_WIKI_DIR`, `ZEN_SKILLS_DIR`, `ZEN_TOOLS_DIR`, `ZEN_MCP_CONFIG`) and `ZEN_BIND`, and works in `<dev home>/workspace`; `ZEN_DEV_REAL=1` keeps the service's keys and folders;
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
| e2e kernels | `http://127.0.0.1:18377` (`ZEN_E2E_PORT`) + 20 per scenario, database `zen_e2e_<pid>_<n>` |
| Eval kernels | `http://127.0.0.1:18301` (`ZEN_EVAL_PORT`) and the next ports, one per run at once (`--jobs`); one database `zen_eval_<run>_<n>` per run |
| SearXNG | external service selected by `ZEN_SEARXNG_URL` |
| Health | `GET /health` on any kernel: `ok`, `db`, `mind`, `workers`, `busy`, `commit`, `version` |

Talk to the dev kernel with the CLI you just built:

```bash
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen ask --json -m faux/smoke "run the smoke command"
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen        # interactive
```

`ZEN_FAUX` is not set by `dev.sh`; pass it as above (or have it in `~/.zenbot/env`) to get `faux/smoke`. Without it, the dev kernel serves real models on the owner's subscriptions.

The dev kernel's tools work in `~/.zenbot-dev/workspace` (`<dev home>/workspace`); with `ZEN_DEV_REAL=1` they work where the service's do (`ZEN_WORKSPACE`, else `$HOME`), on real files.

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

The script installs hooks, fetches/builds binaries, runs unit checks and a copied-database faux smoke. `--check` installs nothing.
A normal run stages the tested binaries and schedules `scripts/apply-upgrade.sh` through systemd.

The installer serializes upgrades, waits for idle, backs up pending schema changes and installs
staged binaries with compatible `.prev` copies. PostgreSQL cutover requires a final quiescent
snapshot/import; the retained source remains recovery evidence. A healthy SQLite installation
records its backend so rollback cannot run a PostgreSQL-only binary against stale state.
Schema rollback never restores the database. Results/recovery: `~/.zenbot/upgrade.log`. No automatic Docker startup.

## Database changes and existing-data migration

SQLite migrations live in `crates/zend/sqlite-migrations/`; never edit an applied migration.
Historical PostgreSQL migrations in `crates/zend/migrations/` remain immutable migration evidence.
Schema changes stay expand-only so an older SQLite binary can run against a newer schema.
`storage::connect` sets WAL, full synchronization, foreign keys and a bounded busy timeout;
`storage::begin` reserves the writer before reading state that will change. Keep network/model
calls outside write transactions. UUIDs are 16-byte blobs, JSON/arrays are text and vectors are
little-endian float32 blobs.

`scripts/storage.py` provides portable, versioned JSONL archives and consistent database backups:

```bash
scripts/storage.py export ~/.zenbot/zen.sqlite3 /private/path/state.jsonl
scripts/storage.py restore /private/path/state.jsonl /private/path/restored.sqlite3
scripts/db.sh pending
scripts/db.sh backup manual
```

An archive includes every durable application table, schema metadata, a completion footer,
counts and digest. Restore verifies the footer, schema, foreign keys, row counts and tape chain,
rebuilds FTS, then publishes a new database atomically. Existing destinations are never overwritten.
The private archive also contains embeddings and security evidence: protect it like the database.
Back up prompt/config files, curated Markdown and retained sources separately.

Existing PostgreSQL data is exported from one repeatable-read snapshot. The exporter can call
`psql` on the host or through the retained container with `--psql-command`; credentials must come
from its environment, not command text. The final cutover requires all writers to be quiescent.
It imports into a new file, copies vectors, retains canonical PostgreSQL payload bytes for old
hashes, and checks integrity before changing the service's database URL. Keep PostgreSQL and its
backups until recovery is verified. Never return to its stale snapshot after SQLite accepts writes.
Starting this kernel with a PostgreSQL URL fails explicitly; it never starts on an empty substitute.

Database backups use SQLite's backup API, including committed WAL contents. Do not copy only a
live main file or delete its WAL. Restore into a new path, verify it with the application, then
switch over while the service is stopped. Live service changes use the upgrade mechanism;
never restart the kernel manually from its own session.

## Harness evals

Harness changes get an eval before they merge; it is advisory (AGENTS.md, "Making a change").
Commands, which tasks run, isolation, caching and the report: [evals/README.md](evals/README.md).

```bash
scripts/eval.sh --plan    # which tasks the change affects, and why; runs nothing
scripts/eval.sh           # those tasks: this checkout (new) vs the installed version (base)
```

Without `--model` it asks the running service (`/api/models`) for its default model, so the service
must be up. Real models use the owner's subscription, so `--jobs` also sets how many turns run on it
at once.

## Scheduled jobs

How the scheduler and its jobs work: DESIGN.md, "Scheduled jobs".

```bash
zen jobs                                      # every job: schedule, next run, last result
zen jobs runs [name]                          # recent runs and their reports
zen jobs run sleep                            # run a job now (here: the memory sleep)
zen jobs add morning-brief -s "0 7 * * 1-5" -p "…"   # an agent job (America/Sao_Paulo unless --tz)
zen jobs pause|resume|rm <name>
```

Throwaway kernels must not run the copy's jobs (an engine update on the real CLIs, agent jobs on real models): `upgrade.sh`'s smoke kernel, the e2e and eval kernels set `ZEN_JOBS=0`, and so does `dev.sh` unless you set `ZEN_JOBS=1`. The `jobs` e2e scenario turns it on with `ZEN_JOBS_TICK=0.5` so runs start within a second.

## Memory sleep

What the sleep does: DESIGN.md, "Memory and skills". `zen memory sleep` runs it now.

```bash
zen memory                                   # short-term memory and the last sleep
zen memory --tier archived                   # what the sleeps archived
zen jobs runs sleep                          # when it ran and what it did
```

A prompt file's old versions are in `~/.zenbot/backups/prompt-files/<name>-<time>.md`; to undo a
change, copy one back.

## Search index

`search::index_loop` indexes sessions, memories, wiki and records every `ZEN_INDEX_SECS` (20 s).
FTS5 and exact identifier search need no provider. Embeddings require `OPENROUTER_API_KEY` and
private-content permission (`ZEN_S1_PRIVATE`); `ZEN_EMBED=0` disables them. Dev drops paid keys;
e2e uses a stub. Inspect `search_docs` and `search_queue` with `scripts/sqlite-query.py` on the
isolated dev database. sqlite-vec is bundled; no PostgreSQL extensions or daemon are needed.

## Wiki

Knowledge: `ZEN_WIKI_DIR` (default `<zen home>/global/wiki`); `zen brain migrate_legacy` previews,
`--args '{"apply":true}'` migrates. Exact originals stay private in `_legacy/`; conflicts stay held.

## Workshop: skills the agent improves

`save_skill` changes remain drafts in `global/skills/_proposed/` until owner fingerprint review:

```bash
zen skills                                   # skills, use and draft fingerprints; legacy tool artifacts
zen skills accept work/release-notes 3f9a0c1d2e4b  # activate the draft reviewed (reject archives it)
git -C ~/.zenbot-dev/global/skills log --oneline   # every change the dev kernel made to its skills
```

Made tools are retired (D-070): `global/tools/` stays inspectable; approval cannot reactivate it.
Use skills with scripts (DESIGN.md, "Memory and knowledge"); e2e: `workshop`, `wiki`.

## Delegation and the routing policy

`delegate` runs subtasks as subagent sessions (`kind = 'subagent'`, hidden from `zen sessions`), several at once with `tasks`. Unless a task names a model, the routing policy picks one per kind of work; the kind comes from System One, so without `ZEN_S1_MODEL` every task is `unknown` and uses the `default` route (else the parent's model).

```bash
zen policy                                        # routes, evidence per kind and model, suggestions
zen policy set default faux/smoke                 # route a kind of work (here every unknown one)
zen policy set build claude/claude-opus-5-5 --candidates codex/gpt-6.1-sol --explore 0.1
zen policy undo                                   # a new version with the previous data
```

Every choice is a `decisions` row (`point = 'model'`); the owner's verdict on the parent session is the evidence. The nightly `routes` job switches a route only on clear evidence (`ZEN_POLICY_MIN_JUDGED` judged subtasks per model, default 20), as a new policy version. The `delegation` e2e scenario (`scripts/e2e/delegate.json`) shows it without a subscription.

## Engine updates

The Claude Code and Codex CLIs track their latest versions. The kernel's `engines` job runs `scripts/update-engines.sh` daily (04:00 UTC), with the kernel's secret variables removed from its environment. Each update downloads from the vendor, checks the SHA-256, runs one real tool-free completion through `zen-engine` (for Codex, also `zen-engine codex-features`: every Codex feature on by default must be one zenbot turns off or has reviewed), and rolls back if it fails. The feature check also runs every day on its own and makes the job fail (exit 1) when it doesn't pass. It never commits, builds or restarts zenbot.

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

`.github/workflows/ci.yml` has four jobs (there is no separate release workflow). Third-party actions are pinned by commit SHA, the tag in a comment beside it; bump them deliberately.

- **`check`**: on every pull request and every push to `main`: `scripts/check.sh` (above). A newer push to a pull request cancels its running check.
- **`postgres-migration`**: exports a disposable PostgreSQL/pgvector fixture through the historical schema and verifies every durable table in the SQLite import. Docker is a CI fixture only.
- **`matrix`**: on the same events as `check`, beside it. Build, unit tests and clippy of `crates/zen-matrix` (its own workspace); not in the release tarball, and `release` doesn't wait for it.
- **`release`**: only on a push to `main` that passed `check` and `postgres-migration`. Builds the `dist` profile (fat LTO), checks `zen-engine` answers `ping`, `zen --version` runs and the binaries fit their `dist` size budgets (`scripts/size-budget.sh target/dist`), runs the e2e scenarios on those `dist` binaries (`ZEN_E2E_BIN=target/dist`), and uploads `zenbot-x86_64-linux-<sha12>.tar.gz` plus `.sha256` to the rolling `edge` prerelease (newest 20 builds kept).

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

## Git flow and rules

How a change is branched, committed, shipped and merged, and the hard rules (never restart `zend`
yourself, expand-only migrations, no secrets), are in `AGENTS.md` ("Making a change", "Rules").

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
| `could not copy the live database` | Check the configured SQLite path, permissions and available disk space |
| Upgrade scheduled but nothing changed | `~/.zenbot/upgrade.log`: it waits for `"busy":0`, up to 30 minutes |
| `upgrade FAILED health check; rolling back` | the service logs copied into `upgrade.log`; fix and re-run. If migrations ran, the log has the restore command |
| `ROLLBACK ALSO UNHEALTHY` | `journalctl -u zenbot -n 50`; tell the owner |
| e2e `kernel did not start` | the kernel log path it printed; check the port isn't taken and the SQLite path is writable |
| e2e check fails on the read-only shell | `bubblewrap` missing |
| e2e `reach` or `wiki` fails to start its server | `python3` missing, or the scenario's port + 1 / + 2 is taken |
| an e2e check fails only with others running | timing under load: rerun it alone (`ZEN_E2E_JOBS=1 scripts/e2e.sh <name>`) and make the check wait for the state (`poll`, `until_ok`) instead of sleeping |
| `web_search failed: … searxng at http://127.0.0.1:8888` | Configure a reachable external `ZEN_SEARXNG_URL` or a Brave/Tavily key |
| `find_tools` lists problems | `~/.zenbot/mcp.json` (invalid JSON, a server with both or neither of `command`/`url`, an unset `${VAR}`, or a secret-named one the server doesn't list in `allow_env`); `GET /api/mcp` shows the same |
| `search` finds nothing recent | the indexer runs every `ZEN_INDEX_SECS`; `search` also indexes before it queries. Warnings `search index:` / `embedding search documents:` in the kernel log |
| A build is killed with no compiler error | out of memory: `CARGO_BUILD_JOBS=2`, one build at a time |
| Upgrade compiles instead of downloading | local changes under `crates/` or `Cargo.*`, or CI hasn't published this commit yet (`scripts/fetch-release.sh --check HEAD`) |
| `zen` says `no token` | `~/.zenbot/token` missing; `install.sh` creates it |
| Engine shows `rolled back` or `skipped` in `zen status` | `engines:` lines in `upgrade.log`; `skipped: check fails before updating` usually means signed out (`zen login`, needs the owner) |
