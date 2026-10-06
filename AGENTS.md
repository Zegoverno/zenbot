# Working on zenbot

This file is for agents changing zenbot's own code, including zenbot itself. Read it fully before making changes.

## What runs where

| Piece | Path | Language | Runs as |
|---|---|---|---|
| Kernel `zend`: API, WebSocket, sessions, tools, auth, worker routing | `crates/zend` | Rust | systemd service `zenbot`, binary `~/.zenbot/bin/zend` |
| CLI `zen`: interactive terminal app and script commands | `crates/zen` | Rust | `~/.zenbot/bin/zen` (linked from `~/.local/bin/zen`) |
| Worker `zen-engine`: runs turns on the Claude Code and Codex CLIs (subscriptions) | `crates/zen-engine` | Rust | child process of `zend` |
| Worker `zen-mind` (optional, `pi`): Pi agent loop, direct ChatGPT sign-in | `packages/mind` | TypeScript (Node 22, from source) | child process of `zend` when `ZEN_WORKERS` includes `pi` |
| Web UI (frozen) | `crates/zend/web/index.html` | HTML/JS | served by `zend` |
| Database | `deploy/compose.yaml` | Postgres + pgvector | Docker |
| Design and plan | `SPEC.md`; current plan in `docs/redesign.md` (read it before starting new work); worker protocol in `docs/worker-protocol.md` | | |

The kernel owns all state and executes every tool call. Workers hold no state: they get the context for a turn and ask the kernel to run tools. Engines run with their own tools switched off (Claude Code `--tools ""`, Codex shell disabled) so every action goes through the kernel. Keep it that way.

Config lives in `~/.zenbot/`: `env` (service environment, including `ZEN_WORKERS`), `token` (API token), `auth.json` (Pi's ChatGPT sign-in, secret, never print it; Claude Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`), `history` (prompt history), `upgrade.log`, `version`, `engines.json` (engine versions from the last update check).

The Claude Code and Codex CLIs are kept on their latest versions by `scripts/update-engines.sh`, run daily by `zen-engines.timer`: each update must pass a real test turn or it is rolled back, and nothing is committed, built or restarted. Pi is different: it is part of the harness, so `packages/mind` pins its exact version and a bump is a normal commit with an eval (below); the daily job only reports a newer Pi (`zen status`). Code that depends on a CLI's flags or output should fail loudly, so the post-update check catches it.

## Making a change

1. Read the code you're changing first. Keep the existing style. Keep changes small.
2. Build and check: `cargo build --release`, `cargo test --release`, `cargo clippy --release --all-targets -- -D warnings`, and `scripts/e2e.sh` (end-to-end scenarios with the scripted model on a throwaway database; CI runs all four on every pull request and push to `main`, and publishes binaries only for commits that pass; add a scenario when you change the kernel's behavior). Test kernel behavior end to end without a subscription using the scripted `faux/smoke` model (`ZEN_FAUX=1`, see `docs/worker-protocol.md`). If you change the Pi worker, `node packages/mind/src/main.ts` must start (Node runs TypeScript directly by stripping types, so don't use TypeScript-only syntax like enums or constructor parameter properties). Changes to the worker protocol must update `docs/worker-protocol.md` and every worker.
3. Test the changed behavior for real where you can, for example `./target/release/zen ask --json "…"` against the running service.
4. Apply it with `scripts/upgrade.sh`. It rebuilds (or, for a clean checkout of a commit CI has built, downloads the binaries via `scripts/fetch-release.sh`), checks, runs a scripted test turn, and schedules the restart for when no session is working, so it is safe to run from inside your own session. The restart will end the current turn's connection; the user reconnects by sending the next message.
5. Afterwards, check `~/.zenbot/upgrade.log`. If the new version wasn't healthy it was rolled back automatically; read the log, fix, and run the script again.
6. Commit with a clear message once the change works. Ask the owner before pushing.

Changes to the harness (system prompt, history, tools, workers, model or effort handling) also get an eval before they are committed: run `scripts/eval.sh` (this checkout against the installed version, same model), show the owner the report, and ask whether to commit. The report informs the owner's decision; it is never a pass/fail gate. See `evals/README.md`.

## Conventions

- One concern per commit. The subject says what changed; the body says why, and what was tested.
- Design decisions written down in `SPEC.md` or in a module's header comment change only with the owner's OK. If a change needs one, ask first, then update the doc in the same commit.
- UI changes (`crates/zen/src/tui.rs`, `editor.rs`) come with render or key tests: build an `App` at a fixed size with output captured (see the tests at the bottom of `tui.rs`).
- Keep doc comments attached to the item they describe, and update them when behavior changes.
- Commits you make in a zen session get `Zen-Session` and `Co-Authored-By` trailers automatically (`scripts/git-hooks`); don't remove them.

## Rules

- **Never** run `systemctl restart zenbot` or kill `zend` yourself: that kills the session you're running in. Always use `scripts/upgrade.sh`.
- Database changes go in a new file in `crates/zend/migrations/` (never edit an applied migration). Migrations are **expand-only**: add tables, columns and indexes; don't drop, rename or change the type of anything in the same release that stops using it (do that in a later release). A rollback swaps the binaries back but not the schema, so the previous build must keep working on the new schema.
- `scripts/upgrade.sh` tries new migrations on a throwaway copy of the live database (`--check` stops after that test), and the install backs the live database up to `~/.zenbot/backups/` (last 10 kept) before applying any. A rollback doesn't restore it; `~/.zenbot/upgrade.log` says how to. `scripts/db.sh pending` lists migrations the live database hasn't applied; `scripts/db.sh backup` takes a backup by hand.
- Don't add dependencies without a good reason; prefer the standard library and what's already used.
- Don't put secrets in the repo, logs or tool output.
- Update `SPEC.md` when a change affects the architecture, and `README.md` when it affects how people install or use zen.

## Useful commands

```bash
zen status                       # health of kernel, database, worker, sign-in
journalctl -u zenbot -n 50       # service logs
cat ~/.zenbot/upgrade.log        # upgrade results
scripts/upgrade.sh --check       # build, check and smoke test without installing
scripts/update-engines.sh --check  # engine versions installed vs latest
./scripts/dev.sh                 # dev kernel in the foreground on :18100 with its own zen_dev database
```
