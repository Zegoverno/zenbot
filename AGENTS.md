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
| Design and plan | `SPEC.md`; worker protocol in `docs/worker-protocol.md` | | |

The kernel owns all state and executes every tool call. Workers hold no state: they get the context for a turn and ask the kernel to run tools. Engines run with their own tools switched off (Claude Code `--tools ""`, Codex shell disabled) so every action goes through the kernel. Keep it that way.

Config lives in `~/.zenbot/`: `env` (service environment, including `ZEN_WORKERS`), `token` (API token), `auth.json` (Pi's ChatGPT sign-in, secret, never print it; Claude Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`), `history` (prompt history), `upgrade.log`, `version`.

## Making a change

1. Read the code you're changing first. Keep the existing style. Keep changes small.
2. Build and check: `cargo build --release` and `cargo test --release`. If you change the Pi worker, `node packages/mind/src/main.ts` must start (Node runs TypeScript directly by stripping types, so don't use TypeScript-only syntax like enums or constructor parameter properties). Changes to the worker protocol must update `docs/worker-protocol.md` and every worker.
3. Test the changed behavior for real where you can, for example `./target/release/zen ask --json "…"` against the running service.
4. Apply it with `scripts/upgrade.sh`. It rebuilds, checks, and schedules the restart for when no session is working, so it is safe to run from inside your own session. The restart will end the current turn's connection; the user reconnects by sending the next message.
5. Afterwards, check `~/.zenbot/upgrade.log`. If the new version wasn't healthy it was rolled back automatically; read the log, fix, and run the script again.
6. Commit with a clear message once the change works. Ask the owner before pushing.

## Rules

- **Never** run `systemctl restart zenbot` or kill `zend` yourself: that kills the session you're running in. Always use `scripts/upgrade.sh`.
- Database changes go in a new file in `crates/zend/migrations/` (never edit an applied migration).
- Don't add dependencies without a good reason; prefer the standard library and what's already used.
- Don't put secrets in the repo, logs or tool output.
- Update `SPEC.md` when a change affects the architecture, and `README.md` when it affects how people install or use zen.

## Useful commands

```bash
zen status                       # health of kernel, database, worker, sign-in
journalctl -u zenbot -n 50       # service logs
cat ~/.zenbot/upgrade.log        # upgrade results
./scripts/dev.sh                 # run a dev kernel in the foreground (stop the service first, or set ZEN_PORT)
```
