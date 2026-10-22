# zenbot: local development loop

How to build, test, try and ship a change. Rules: [AGENTS.md](AGENTS.md); every file, route,
setting and script: [MAP.md](MAP.md). A change passes the same checks as CI locally before its pull
request, and is installed only through `scripts/upgrade.sh`.

## Prerequisites

`install.sh` sets up `git`, `curl`, `jq`, `python3`, `bubblewrap` (sandboxed shells, e2e),
`poppler-utils` and Node.js 22 (`~/.local/node`, for Codex), and Rust only when it has to compile.
Add clippy (`rustup component add clippy`) and `gh` yourself.

The kernel embeds SQLite with FTS5 and sqlite-vec: no database daemon or Docker to build, test or
upgrade. `DATABASE_URL` must name SQLite (default `sqlite://<ZEN_HOME>/zen.sqlite3`).

This VM (2 CPUs, 7.7 GB) has OOM-killed parallel release builds: keep `CARGO_BUILD_JOBS=2`
(`scripts/check.sh` sets it) and never run two release builds at once.

## The loop at a glance

```bash
git switch -c fix/short-name                     # 1. branch
scripts/check.sh memory                          # 2. as CI, but only e2e scenarios matching "memory"
scripts/check.sh                                 #    everything (before the pull request)
ZEN_FAUX=1 scripts/dev.sh                        # 3. dev kernel (second terminal), then:
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen ask --json -m faux/smoke "hi"
scripts/eval.sh --plan && scripts/eval.sh        # 4. harness change: eval, show the owner the report
scripts/upgrade.sh && tail -20 ~/.zenbot/upgrade.log   # 5. install on this box
# 6. commit, push, open a PR; merge on green CI
```

## Build, test, lint (as CI runs them)

`scripts/check.sh` is CI's `check` job. It runs these steps in order, timing each and stopping at
the first failure:

| Step | Command |
|---|---|
| `docs` | `scripts/check-docs.py --base origin/<base>` (first: a missing PROGRESS.md entry fails fast) |
| `s1-guard` | no System One call bypasses the S1 engine (`crates/zend/src/s1/`) |
| `build` | `cargo build --release --locked` |
| `py-sqlite`, `storage`, `pg-archive`, `cutover` | `scripts/test-sqlite.sh`, then `scripts/test_storage.py`, `scripts/test_postgres_archive.py`, `scripts/test_cutover.py` |
| `tests` | `cargo test --release --locked` |
| `clippy` | `cargo clippy --release --locked --all-targets -- -D warnings` |
| `sizes` | `scripts/size-budget.sh` (raise a budget only on purpose, saying why) |
| `e2e` | `ZEN_E2E_NO_BUILD=1 scripts/e2e.sh [filter]` |

With a filter only the matching e2e scenarios run; run it unfiltered (or let CI) before merging.

## End-to-end scenarios

`scripts/e2e.sh` builds (unless `ZEN_E2E_NO_BUILD=1`) and runs each scenario on fresh kernels from
this checkout (or `ZEN_E2E_BIN`) with the faux model, each with its own database, workspaces, `HOME`
and ports, `ZEN_E2E_JOBS` (default 4) at once. No subscription, no internet, live service untouched.
Every scenario ends by checking each session's tape is gapless and its hash chain recomputes.

```bash
scripts/e2e.sh search          # scenarios whose name or part function contains "search"
ZEN_E2E_KEEP=1 scripts/e2e.sh  # keep databases and files (it prints where)
```

Scenarios are the `run …` lines at the bottom; the comment above each function says what it checks.
Extend one when you change kernel behavior; add one only for a new setup (no kernel needed: unit test):

1. If the turn needs a scripted model, add `scripts/e2e/<name>.json` (format below); `script <file>
   's/ROUTE/bounded/'` fills placeholders.
2. Write a function: `ws=$(new_workspace <name>)`; `start_kernel "$ws" "$(script <file>.json)"
   [ENV=VALUE …]` (`""` for the default faux turn); `zen ask --json -m faux/smoke "…"` (`-s "$sid"`
   continues); `q "SQL"` queries the scenario's database; `check "description" <command…>`, `eq`,
   and `poll`/`until_ok` to wait for state instead of sleeping. Test servers use `$PORT` + 1…19.
3. Register it with `run <name> <function…>`.

## The faux model

With `ZEN_FAUX=1`, `zen-engine` also offers `faux/smoke`, a scripted model that drives a real turn
(default: one `bash` call, then an answer). `ZEN_FAUX_SCRIPT` names a JSON file of steps
(`{"tool": "bash", "args": {…}}`, `{"text": "…"}`, `{"sleep": 5}`, `{"exit": 1}`, optionally with
`"when": "text"` or `"ignore_abort": true`), or step lists keyed by session kind (`verify`,
`default`). Examples: `scripts/e2e/`; format: `docs/worker-protocol.md` ("Testing without a model").

## The dev kernel

`ZEN_FAUX=1 scripts/dev.sh` builds and runs a kernel from this checkout in the foreground, next to
the service. It loads `~/.zenbot/env` and the token but has its own home `~/.zenbot-dev`
(`ZEN_DEV_HOME`), database (`ZEN_DEV_DB`), workspace and port (`ZEN_DEV_PORT`), drops the paid keys,
folder settings and `ZEN_BIND` (`ZEN_DEV_REAL=1` keeps them), records this commit as `ZEN_HARNESS`
and runs no scheduled jobs unless `ZEN_JOBS=1`. Without `ZEN_FAUX` it serves real models.

```bash
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen ask --json -m faux/smoke "run the smoke command"
ZEN_URL=http://127.0.0.1:18100 ./target/release/zen          # interactive
```

Ports: service 8100 (`ZEN_PORT`), dev 18100, upgrade smoke 18199 (`ZEN_SMOKE_PORT`), e2e 18377 + 20
per scenario (`ZEN_E2E_PORT`), evals 18301 and up (`ZEN_EVAL_PORT`); each non-live kernel has its own
`ZEN_HOME` and database.

Throwaway kernels run with `ZEN_JOBS=0`. `web_fetch` reaches only public addresses. The installed
service runs installed binaries: try CLI changes with `./target/release/zen` against it, kernel or
worker changes on the dev kernel. `zen ask --json` prints one object and exits non-zero on failure.

## Applying a change: `scripts/upgrade.sh`

The only way to install a change. **Never** run `systemctl restart zenbot` or kill `zend` yourself:
that kills the session you are running in. `upgrade.sh` is safe from inside a zen session.

```bash
scripts/upgrade.sh           # build (or fetch CI's binaries), check, smoke test, schedule the install
scripts/upgrade.sh --check   # build, check and smoke test only; installs nothing
```

It fetches or builds the binaries, smoke-tests them on a copy of the live database and schedules `scripts/apply-upgrade.sh`, which waits until no session is working (up to 30 minutes),
backs up the database if migrations are pending, installs, health-checks and puts the previous
binaries back if unhealthy (never the database). It also rebuilds an installed `zen-matrix` whose
sources changed. Results: `~/.zenbot/upgrade.log`.

On a PostgreSQL installation the first upgrade is the cutover: with the kernel idle it stops it,
archives, imports and verifies into SQLite, then switches `DATABASE_URL` and the service. A failure
before SQLite starts restores the old installation; after that there is no going back to PostgreSQL
(its data and the archive are kept).

## Database changes and existing-data migration

Migrations go in a new file in `crates/zend/sqlite-migrations/` (the PostgreSQL ones in
`crates/zend/migrations/` are history). Never edit an applied one; keep changes expand-only so the
previous binary runs on the newer schema. `storage::begin` takes the write lock before reading state
it changes; keep network and model calls outside write transactions.

```bash
scripts/db.sh pending                                         # migrations not yet applied
scripts/db.sh backup manual                                   # online backup (WAL included)
scripts/storage.py export ~/.zenbot/zen.sqlite3 /private/path/state.jsonl
scripts/storage.py restore /private/path/state.jsonl /private/path/restored.sqlite3
scripts/sqlite-query.py ~/.zenbot-dev/zen_dev.sqlite3 "SELECT count(*) FROM sessions"
```

`restore` verifies the archive (footer, counts, digest, foreign keys, tape chain), rebuilds FTS and
publishes a new file, never overwriting one. Archives hold secrets: protect them like the database.
Don't copy a live database file by hand or delete its WAL. `scripts/migrate-storage.sh --offline`
does a PostgreSQL export and import by hand with the service stopped (credentials from the
environment, never arguments). The kernel refuses a PostgreSQL URL.

## Harness evals

Harness changes get an advisory eval before they merge (AGENTS.md); `scripts/eval.sh` uses the
running service's default model unless `--model`. Everything else: [evals/README.md](evals/README.md).

## Engine updates

The daily `engines` job (04:00 UTC) runs `scripts/update-engines.sh` without the kernel's secrets:
each new Claude Code or Codex CLI is checksum-verified, must pass a real tool-free turn through
`zen-engine` (Codex also `zen-engine codex-features`: no unreviewed default-on feature), else it is
rolled back. It never builds or restarts zenbot.

```bash
scripts/update-engines.sh --check                 # installed vs latest; changes nothing
scripts/update-engines.sh                         # update now (safe inside a session)
ZEN_ENGINES=claude scripts/update-engines.sh      # only some engines
ZEN_ENGINES_FAIL=codex scripts/update-engines.sh  # force a failure, to test the rollback
zen jobs runs engines
```

Results: `~/.zenbot/upgrade.log` (`engines:` lines) and `~/.zenbot/engines.json` (`zen status`).

## CI and releases

`.github/workflows/ci.yml` (actions pinned by commit SHA; bump them deliberately):

- **`check`**: every pull request and push to `main`: `scripts/check.sh`.
- **`postgres-migration`**: migrates a disposable PostgreSQL fixture (the historical schema) to
  SQLite and verifies every durable table. Docker is only a CI fixture here.
- **`matrix`**: build, tests and clippy of `crates/zen-matrix`.
- **`arm64`**: on pull requests, builds and unit-tests the aarch64 build.
- **`release`**: on a push to `main` after `check` and `postgres-migration`, for x86_64 and aarch64
  (glibc 2.35): builds the `dist` profile, runs size budgets and e2e on it, and uploads
  `zenbot-<arch>-linux-<sha12>.tar.gz` to the rolling `edge` prerelease.

`install.sh`, `upgrade.sh` and `eval.sh` download these (`scripts/fetch-release.sh`), else compile.

```bash
scripts/fetch-release.sh --check origin/main && echo "binaries published"
gh run list --limit 5
```

## The Matrix channel

`crates/zen-matrix` is its own Cargo workspace, so the commands above don't build it:

```bash
cd crates/zen-matrix && cargo build --release --locked && cargo test --release --locked && cargo clippy --release --locked --all-targets -- -D warnings
scripts/matrix-e2e.sh   # end to end through E2EE: throwaway homeserver in Docker, faux kernel (not in CI)
scripts/matrix.sh       # build, test, install, sign in, restart zen-matrix (--build: build and test only)
```

The first build takes about 15 minutes on 2 cores. Setup: [docs/matrix.md](docs/matrix.md).

## Troubleshooting

```bash
zen status                              # kernel, database, workers, sign-in, engines, jobs
curl -s http://127.0.0.1:8100/health    # ok, db, mind, workers, busy, version, commit
journalctl -u zenbot -n 50              # service logs
tail -50 ~/.zenbot/upgrade.log          # upgrades, rollbacks, engine updates
```

| Symptom | Look at |
|---|---|
| Build killed with no compiler error | out of memory: `CARGO_BUILD_JOBS=2`, one build at a time |
| `SMOKE TEST FAILED` | the kernel log lines it printed; often a migration failing on the live copy |
| Upgrade scheduled, nothing changed | `upgrade.log`: it waits for `"busy":0`, up to 30 minutes |
| `FAILED health check; rolling back` / `ROLLBACK ALSO UNHEALTHY` | service logs in `upgrade.log`; for the second, tell the owner |
| Upgrade compiles instead of downloading | local changes under `crates/`, or CI hasn't published yet |
| e2e `kernel did not start` | the kernel log it printed: port taken, SQLite path not writable |
| e2e fails only under load | rerun alone (`ZEN_E2E_JOBS=1 scripts/e2e.sh <name>`); wait with `poll`/`until_ok` |
| `find_tools` lists problems | `~/.zenbot/mcp.json`; `GET /api/mcp` shows the same |
| Engine `rolled back` / `skipped` in `zen status` | `engines:` lines in `upgrade.log`; usually signed out (`zen login`, needs the owner) |
