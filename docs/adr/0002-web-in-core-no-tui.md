# ADR 0002: The web app joins zenbot; the TUI goes

Status: accepted (2026-11-17); implemented: the web app lives in `web/` and the TUI is gone (proposed 2026-11-14)

## Context

zenbot has two full UIs that do the same things:
- the TUI (`crates/zen/src/tui`, about 9.4k lines, plus `editor.rs`);
- the web app (a separate private repo with a Svelte page and a Node server that relays the kernel).

Every feature is built twice. The writer, the reader and Sudoku are moving to plugins (ADR 0001),
so what is left in either UI is the shell.

## Decision

- **zenbot's UIs:**
  - the **web UI** (moved from its own repo into this one as `web/`);
  - the `zen` **CLI** for scripts (`ask`, `jobs`, `memory`, `status`, `plugins`…);
  - **Matrix**.

  All three take the same commands from `crates/zen-client/src/commands.rs`. In the web chat and
  in Matrix they are slash commands.
- **The TUI and `zen chat` are removed** once the web UI covers everything they do (checklist in
  `docs/plan.md`).
- **Move first, then shrink.** `web/` arrives as it is, Node server included, and is served as
  today. Then:
  - the writer-only server parts (`ai.ts`, `check.ts`, `versions.ts`, `findnotes.ts`,
    `changelog.ts`) go to the writer plugin;
  - Sudoku goes to its plugin;
  - what remains (the kernel relay, home files, commands, uploads) is either served by `zend`
    directly, or kept as a thin Node server if that turns out simpler. This is decided when we get
    there, with measurements.
- **The old web app repo is archived** after the move. Its history stays there; the public plugin
  repos start with fresh history.

## Consequences

- One UI to build and test. UI tests become the web e2e tests (Playwright), not TUI render tests.
- `scripts/check.sh` and CI also run the web checks.
- `upgrade.sh` installs the web build together with the kernel, so the two can't drift apart.
- People who used the terminal app keep the CLI. The terminal is no longer an interactive UI.
