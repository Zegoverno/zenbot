# Working on zenbot

This file is for agents (and people) changing zenbot's own code, including zenbot itself. Read it
fully before making changes.

## Read first

1. [CONTEXT.md](CONTEXT.md): what zenbot is for and how success is measured.
2. [ROADMAP.md](ROADMAP.md): the active phase and its next step. Start new work from there.
3. [MAP.md](MAP.md): find what you'll touch, what it depends on and what depends on it. Then read the
   code itself.
4. [DESIGN.md](DESIGN.md) for how the system works, [DECISIONS.md](DECISIONS.md) before revisiting a
   choice, [DEVELOPMENT.md](DEVELOPMENT.md) for every command below.

## What runs where

| Piece | Path | Runs as |
|---|---|---|
| Kernel `zend`: API, WebSocket, sessions, tools, auth, worker routing | `crates/zend` (Rust) | systemd service `zenbot`, binary `~/.zenbot/bin/zend` |
| CLI `zen`: terminal app and script commands | `crates/zen` (Rust) | `~/.zenbot/bin/zen` (linked from `~/.local/bin/zen`) |
| Worker `zen-engine`: turns on the Claude Code and Codex CLIs | `crates/zen-engine` (Rust) | child process of `zend` |
| Shared types `zen-proto`: the worker protocol's messages | `crates/zen-proto` (Rust library) | linked into `zend` and `zen-engine` |
| Channel `zen-matrix`: the owner's Matrix rooms ↔ sessions | `crates/zen-matrix` (Rust, its own Cargo workspace) | systemd service `zen-matrix`, binary `~/.zenbot/bin/zen-matrix` |
| Database and web search | `deploy/compose.yaml` | Postgres + pgvector, and SearXNG, in Docker |

The kernel owns all state and executes every tool call. Workers hold no state: they get the context
for a turn and ask the kernel to run tools. Engines run with their own tools switched off (Claude
Code `--tools ""`, Codex shell disabled) so every action goes through the kernel. Keep it that way.

Config lives in `~/.zenbot/` (MAP.md lists every file). `token`, `env`, `matrix.env` and
`matrix/` are secret: never print them.

## Making a change

1. **Branch.** Start from an up-to-date `main` and work on a branch (`feat/…`, `fix/…`, `docs/…`),
   never directly on `main`.
2. **Read the code you're changing.** Keep the existing style. Keep changes small.
3. **Build and check** exactly as CI does: `scripts/check.sh` builds once, then runs the tests,
   clippy as errors, `scripts/e2e.sh` and the docs check, timing each step and stopping at the
   first failure (`scripts/check.sh <filter>` runs only matching e2e scenarios; DEVELOPMENT.md).
   Add an e2e scenario when you change the kernel's behavior. Test kernel behavior without a
   subscription with the scripted `faux/smoke` model. `crates/zen-matrix` is built, tested and
   linted in its own CI job; test it end to end with `scripts/matrix-e2e.sh` (not in CI). Changes
   to the worker protocol update `docs/worker-protocol.md` and every worker.
4. **Test it for real** where you can, e.g. `./target/release/zen ask --json "…"` against the running
   service or a dev kernel.
5. **Update the docs** in the same branch (table below), then run `scripts/check-docs.py`. Docs are
   not a follow-up task. Before you merge, reread what your docs say against the branch's final
   diff: later commits often change what an earlier commit's docs describe.
6. **Apply it** with `scripts/upgrade.sh`, then check `~/.zenbot/upgrade.log`. It is safe to run from
   inside your own session: it restarts when no session is working (after 30 minutes it restarts
   anyway, ending running turns), and rolls back if the new version isn't healthy. It doesn't
   touch `zen-matrix`: `scripts/matrix.sh` builds, installs and restarts that.
7. **Ship.** Commit (one concern per commit), push the branch, open a pull request, merge when CI is
   green. Harness changes (system prompt, history, tools, workers, model or effort handling) also get
   an eval first: run `scripts/eval.sh`, show the owner the report (in the pull request too), and merge
   only once the owner agrees. The report informs the owner's decision; it is never a pass/fail gate.
8. **After a merge**, `git checkout main && git pull` before starting the next branch.

## Update the docs before shipping

| File | Update when |
|---|---|
| `PROGRESS.md` | Always: add a dated entry at the top saying what shipped and why |
| `ROADMAP.md` | A step is done, the active phase changes, a plan or an open decision changes, debt is found or paid |
| `DECISIONS.md` | A decision is made or changed (new `D-NNN` entry; mark what it supersedes) |
| `DESIGN.md` | How the system works changes, or the target design changes |
| `MAP.md` | Files, routes, tables, settings or scripts are added, removed or change role |
| `DEVELOPMENT.md` | The dev loop, scripts, tests or CI change |
| `SPEC.md` | The long-term target of a module changes |
| `CONTEXT.md` | What zenbot is for, the success measure, principles or constraints change |
| `README.md` | How people use zen changes (commands, keys, settings they'd set) |
| `INSTALL.md` | How zen is installed changes (`install.sh`, services, prerequisites) |
| `AGENTS.md` | What runs where, the way changes are made or shipped, or a rule changes |
| `docs/*.md` | The subsystem they describe changes (protocols, context, Matrix, evals in `evals/README.md`) |
| `crates/zend/defaults/` | Tools, paths or the home layout the agent's default prompt files and skills name change |
| Doc comments | The item they describe changes (a module's header comment is its design note) |

### Keeping the docs true

The docs are worth reading only while they match the code, and every session builds on them. So:

- **The code wins.** When a doc disagrees with the code you're reading, fix the doc in the same
  branch, even if it isn't part of your change, and say so in the commit.
- **Present tense means shipped.** Describe what `main` does now. What's planned goes to ROADMAP.md
  (or a "target" section), marked as not built; statuses (`built`, `done`, "next") change when the
  thing merges, not when it's started.
- **History stays history.** PROGRESS.md entries, DECISIONS.md entries and `docs/research/` say
  what was true when written: don't rewrite them; mark a superseded decision `Superseded by D-NNN`.
- **`scripts/check-docs.py`** (CI runs it on every pull request) fails when a route, tool, setting,
  table, event, tape kind, slash command, e2e scenario or crate in the code is missing from the doc
  that lists it, when a current doc names a setting, repo path or decision that doesn't exist, and
  when a change to the code adds no PROGRESS.md entry (`[no-progress]` in a commit message if
  nothing shipped). It can't tell whether a sentence is still true: that's on you. When you add a
  kind of list a doc keeps, add it to the check too.

Keep each fact in one file and link to it from the others. Repo docs are for whoever works on the
code next: no conversation notes, no memory-system link syntax (`[[…]]`), no perishable usage data.
Refer to the owner as "the owner".

## Conventions

- One concern per commit. The subject says what changed; the body says why, and what was tested.
- Design decisions (DECISIONS.md, DESIGN.md, SPEC.md, or a module's header comment) change only with
  the owner's OK. If a change needs one, ask first, then update the doc in the same pull request.
- UI changes (`crates/zen/src/tui/`, `editor.rs`) come with render or key tests: build an `App` at a
  fixed size with output captured (see the tests beside the modules in `tui/`).
- Keep doc comments attached to the item they describe, and update them when behavior changes.
- Commits made in a zen session get `Zen-Session` and `Co-Authored-By` trailers automatically
  (`scripts/git-hooks`, enabled by `install.sh` and `scripts/upgrade.sh`); don't remove them.
- Don't add dependencies without a good reason; prefer the standard library and what's already used.

## Rules

- **Never** run `systemctl restart zenbot` or kill `zend` yourself: that kills the session you're
  running in. Always use `scripts/upgrade.sh`.
- Database changes go in a new file in `crates/zend/migrations/`; never edit an applied migration.
  Migrations are **expand-only**: add tables, columns and indexes; drop, rename or retype only in a
  later release than the one that stops using it. A rollback swaps the binaries back but not the
  schema.
- Don't put secrets in the repo, logs or tool output.
- Code that depends on a CLI's flags or output fails loudly, so the daily engine update check catches
  a breaking change.

## Useful commands

```bash
zen status                  # health of kernel, database, workers, sign-in, engine versions, jobs
journalctl -u zenbot -n 50  # service logs (zen-matrix: journalctl -u zen-matrix)
cat ~/.zenbot/upgrade.log   # upgrade results
scripts/upgrade.sh --check  # build, check and smoke test without installing
ZEN_FAUX=1 scripts/dev.sh   # dev kernel on :18100 with its own zen_dev database
```
