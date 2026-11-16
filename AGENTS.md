# Working on zenbot

For agents (and people) changing zenbot's code, zenbot itself included. MAP.md says how it works
and where things are; each module's header comment is its design note.

## Ground rules

- **The kernel owns all state and runs every tool call.** Workers are stateless and engines run
  with their own tools off, so every action goes through the kernel and onto the tape. Keep it so.
- **The tape is the source of truth.** The model reads only what the tape holds; the prefix
  (instructions and tools) is fixed per session so the prompt cache holds.
- **One command table** (`crates/zen-client/src/commands.rs`): a command added to one client is
  added there, for all of them.
- **Never** run `systemctl restart zenbot` or kill `zend`: that ends the session you're running in.
  Ship with `scripts/upgrade.sh`.
- Schema changes go in a new file in `crates/zend/sqlite-migrations/`; never edit an applied one.
- No secrets in the repo, logs or output: `~/.zenbot/token`, `env`, `matrix.env`, `matrix/` and the
  database are private.

## Making a change

1. **Branch** from an up-to-date `main` (`feat/…`, `fix/…`); never commit to `main`.
2. **Read the code you're changing**; keep its style and keep changes small.
3. **Check:** `scripts/check.sh` (the web app's checks and e2e tests, build, unit tests, clippy
   with warnings as errors, e2e);
   `scripts/check.sh <filter>` runs only the matching e2e scenarios. Test kernel behavior with the
   `faux/smoke` model (scripts in `scripts/e2e/`). `crates/zen-matrix` is its own workspace:
   `scripts/matrix.sh --build`.
4. **Try it for real:** `scripts/dev.sh` runs a dev kernel on :18100 with its own home, or
   `scripts/upgrade.sh` installs your checkout, web app included (rolls back if it isn't healthy).
   The web app in development: `cd web && ZEN_WEB_PORT=4401 npm run dev` (hot reload).
5. **Docs:** fix MAP.md, INSTALL.md or this file in the same branch when your change makes them
   wrong, and the module's header comment when what it does changes. The code wins.
6. **Ship:** one concern per commit (the subject says what, the body why); push, open a pull
   request, merge when `scripts/check.sh` passes (CI runs the same). `scripts/eval.sh` compares
   harness versions when a prompt, tool or engine change needs evidence; it never blocks.

## Conventions

- UI changes (`crates/zen/src/tui/`, `editor.rs`) come with render or key tests (see the tests next
  to the modules); web changes (`web/`, see `web/AGENTS.md`) with an assertion in its e2e tests.
- Commits made from a zen session get `Zen-Session` and `Co-Authored-By` trailers from
  `scripts/git-hooks`; keep them.
- Code that depends on a CLI's flags or output fails loudly, so the daily engine update catches a
  breaking release.
- Don't add dependencies without a good reason.

## Useful commands

```bash
zen status                  # kernel, database, workers, sign-in, engines, jobs
journalctl -u zenbot -n 50  # service logs
cat ~/.zenbot/upgrade.log   # upgrade results
journalctl --user -u zen-web -n 50  # the web app's logs
scripts/upgrade.sh --check  # build without installing
scripts/check.sh            # what CI checks
```
