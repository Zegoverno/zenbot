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
| Web UI (frozen) | `crates/zend/web/index.html` | served by `zend` |
| Database | `deploy/compose.yaml` | Postgres + pgvector in Docker |

The kernel owns all state and executes every tool call. Workers hold no state: they get the context
for a turn and ask the kernel to run tools. Engines run with their own tools switched off (Claude
Code `--tools ""`, Codex shell disabled) so every action goes through the kernel. Keep it that way.

Config lives in `~/.zenbot/` (MAP.md lists every file). `auth.json` may hold a retired Pi
sign-in: secret, never print it. This VM is a development box; the owner uses zenbot for real on
another VM.

## Making a change

1. **Branch.** Start from an up-to-date `main` and work on a branch (`feat/…`, `fix/…`, `docs/…`),
   never directly on `main`.
2. **Read the code you're changing.** Keep the existing style. Keep changes small.
3. **Build and check** exactly as CI does: build, tests, clippy as errors, and `scripts/e2e.sh`
   (DEVELOPMENT.md). Add an e2e scenario when you change the kernel's behavior. Test kernel behavior
   without a subscription with the scripted `faux/smoke` model. Changes to the worker protocol
   update `docs/worker-protocol.md` and every worker.
4. **Test it for real** where you can, e.g. `./target/release/zen ask --json "…"` against the running
   service or a dev kernel.
5. **Update the docs** in the same branch (table below). Docs are not a follow-up task.
6. **Apply it** with `scripts/upgrade.sh`, then check `~/.zenbot/upgrade.log`. It is safe to run from
   inside your own session: it restarts when no session is working, and rolls back if the new
   version isn't healthy.
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
| `README.md` | How people install or use zen changes |
| `docs/*.md` | The subsystem they describe changes |

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
  (`scripts/git-hooks`); don't remove them.
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
zen status                  # health of kernel, database, workers, sign-in, engine versions
journalctl -u zenbot -n 50  # service logs
cat ~/.zenbot/upgrade.log   # upgrade results
scripts/upgrade.sh --check  # build, check and smoke test without installing
ZEN_FAUX=1 scripts/dev.sh   # dev kernel on :18100 with its own zen_dev database
```
