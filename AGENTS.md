# Working on zenbot

For agents (and people) changing zenbot's own code, including zenbot itself. Read it fully first.

## Read first

1. [CONTEXT.md](CONTEXT.md): what zenbot is for and how success is measured.
2. [ROADMAP.md](ROADMAP.md): what's next, open decisions and debt. Start new work there.
3. [MAP.md](MAP.md): what you'll touch and what depends on it. Then read the code itself.
4. [DESIGN.md](DESIGN.md) for how it works, [DECISIONS.md](DECISIONS.md) before revisiting a choice,
   [DEVELOPMENT.md](DEVELOPMENT.md) for every command below.

## What runs where

| Piece | Path | Runs as |
|---|---|---|
| Kernel `zend`: API, sessions, tools, auth, state, worker routing | `crates/zend` | systemd service `zenbot`, `~/.zenbot/bin/zend` |
| CLI `zen`: terminal app and script commands | `crates/zen` | `~/.zenbot/bin/zen` (linked from `~/.local/bin/zen`) |
| Worker `zen-engine`: turns on the Claude Code and Codex CLIs | `crates/zen-engine` | child process of `zend` |
| `zen-proto`: shared message helpers and typed protocols | `crates/zen-proto` | library |
| `zen-client`: the kernel client (token, HTTP, session stream) | `crates/zen-client` | library, in `zen` and `zen-matrix` |
| Channel `zen-matrix`: Matrix rooms ↔ sessions | `crates/zen-matrix` (own Cargo workspace) | systemd service `zen-matrix` |
| Database | `crates/zend/sqlite-migrations/` | private SQLite file with FTS5 and sqlite-vec, inside `zend` |

The kernel owns all state and executes every tool call; workers are stateless. Engines run with their own tools off (Claude Code
`--tools ""`, Codex shell disabled) so every action goes through the kernel. Keep it that way.

Config lives in `~/.zenbot/` (MAP.md lists every file). `token`, `env`, `env.prev`, `matrix.env`,
`matrix/`, the SQLite database with its WAL/SHM files, backups and archives are private: never print
them.

## Making a change

1. **Branch** from an up-to-date `main` (`feat/…`, `fix/…`, `docs/…`); never work on `main`.
2. **Read the code you're changing.** Keep its style; keep changes small.
3. **Check as CI does:** `scripts/check.sh` (`scripts/check.sh <filter>` for only the matching e2e
   scenarios). Extend an e2e scenario (or add one for a new setup) when kernel behavior changes; test
   without a subscription with the `faux/smoke` model. `crates/zen-matrix` has its own CI job and
   `scripts/matrix-e2e.sh`. A worker-protocol change updates `docs/worker-protocol.md`,
   `crates/zen-proto/src/worker.rs` and every worker.
4. **Test it for real** where you can: `./target/release/zen ask --json "…"` against the service or
   a dev kernel.
5. **Update the docs** in the same branch (below) and run `scripts/check-docs.py`. Before merging,
   reread your docs against the final diff: later commits often change what earlier docs describe.
6. **Apply it** with `scripts/upgrade.sh` (safe from inside your own session), then check
   `~/.zenbot/upgrade.log`.
7. **Ship.** One concern per commit; push, open a pull request, merge when CI is green. Harness
   changes (system prompt, history, tools, workers, model or effort handling) get an eval first:
   `scripts/eval.sh --plan` shows the affected tasks, `scripts/eval.sh` runs them (`--full` for a
   large change or when the owner asks). Show the owner the report (in the pull request too) and
   merge only once they agree; the report informs that decision, it is never a pass/fail gate.
8. **After a merge**, `git checkout main && git pull` before the next branch.

## Update the docs before shipping

Every change updates, in the same branch:

| File | When |
|---|---|
| `PROGRESS.md` | Always: a short dated entry at the top saying what shipped and why |
| `DECISIONS.md` | A decision is made or changed (below) |
| `MAP.md` | Files, routes, tools, tables, settings, scripts or e2e scenarios added, removed or changing role |
| `README.md` | How people use zen changes (commands, keys, settings) |
| The module's header comment | What the module does or guarantees changes (it is its design note) |

Any other current doc (DESIGN, ROADMAP, DEVELOPMENT, CONTEXT, INSTALL, `docs/*.md`, this file,
`crates/zend/defaults/`) is fixed when it becomes wrong: when you notice, or when your change makes a
sentence false. `docs/archive/` is history: never updated, not required reading.

**Decisions.** DECISIONS.md is an index of the rules in force; the full text of past decisions is in
`docs/archive/DECISIONS-full.md`. A new decision is a short entry at the top: a heading
`## D-NNN — title`, a status/date line, and the rule in about 8 lines at most. The reasoning goes in
the pull request and PROGRESS.md. When a decision is superseded, its index entry becomes
"Superseded by D-NNN".

### Keeping the docs true

- **The code wins.** When a doc disagrees with the code, fix the doc in the same branch, even if it
  isn't part of your change, and say so in the commit.
- **Present tense means shipped.** Describe what `main` does now. Plans go to ROADMAP.md (or a
  marked "target" section); statuses change when the thing merges, not when it starts.
- **History stays history.** PROGRESS.md entries and `docs/archive/` say what was true when written:
  don't rewrite them. PROGRESS.md entries older than a week or two move to `docs/archive/` when the
  file outgrows its budget.
- **Keep docs short.** One fact in one file, linked from the others; module detail belongs in its
  header comment; history moves to `docs/archive/` instead of growing a doc.
- **`scripts/check-docs.py`** (CI runs it) fails when MAP.md misses a route, tool, setting, table,
  e2e scenario or crate, when a current doc names a setting, repo path or decision that doesn't
  exist, when the current docs exceed their size budget, and when a code change adds no PROGRESS.md
  entry (`[no-progress]` in a commit message if nothing shipped). Whether a sentence is still true
  is on you.

Repo docs are for whoever works on the code next: no conversation notes, no memory-system link
syntax (`[[…]]`), no perishable usage data. Refer to the owner as "the owner".

## Conventions

- One concern per commit. The subject says what changed; the body says why, and what was tested.
- Design decisions (DECISIONS.md, DESIGN.md, a module's header comment) change only with the
  owner's OK: ask first, then update the doc in the same pull request.
- UI changes (`crates/zen/src/tui/`, `editor.rs`) come with render or key tests: an `App` at a fixed
  size with output captured (see the tests beside the modules in `tui/`).
- Commits made in a zen session get `Zen-Session` and `Co-Authored-By` trailers from
  `scripts/git-hooks` (enabled by `install.sh` and `scripts/upgrade.sh`); don't remove them.
- Don't add dependencies without a good reason; prefer the standard library and what's already used.

## Rules

- **Never** run `systemctl restart zenbot` or kill `zend` yourself: that kills the session you're
  running in. Always use `scripts/upgrade.sh`.
- Schema changes go in a new file in `crates/zend/sqlite-migrations/`; never edit an applied
  migration (the PostgreSQL ones in `crates/zend/migrations/` are history). Migrations are
  **expand-only**: add tables, columns and indexes; drop, rename or retype only in a later release
  than the one that stops using it. A rollback swaps the binaries back, not the schema.
- No secrets in the repo, logs or tool output.
- Code that depends on a CLI's flags or output fails loudly, so the daily engine update check
  catches a breaking change.

## Useful commands

```bash
zen status                  # kernel, database, workers, sign-in, engine versions, jobs
journalctl -u zenbot -n 50  # service logs (zen-matrix: journalctl -u zen-matrix)
cat ~/.zenbot/upgrade.log   # upgrade results
scripts/upgrade.sh --check  # build, check and smoke test without installing
scripts/check.sh            # what CI checks
scripts/eval.sh --plan      # which eval tasks a harness change affects, and why
ZEN_FAUX=1 scripts/dev.sh   # dev kernel on :18100 with its own home and database
```
