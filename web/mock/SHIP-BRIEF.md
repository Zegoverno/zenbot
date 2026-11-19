# Ship the redesign: brief

## Intent
Ze (2026-11-19): "use the opened PR #24 to ship the new UI, make it the closest as possible to what
we mocked on this session. add all features needed for things to work. If there are changes needed
to the plugins itself, address them as well. After all PRs are finished and CI green, merge and
install/upgrade everything."

## Goal
The real web app (`web/src/zen`, `web/server`) looks and behaves like the mock (`web/mock`, run it
with `web/scripts/mock.sh`) on real data; the gaps in `web/mock/BUILD-GAPS.md` are closed in the
kernel, web server and plugins; PR #24 (branch `fix/md-bare-urls`, built from `feat/web-redesign`)
and the plugin PRs merge on green CI; zenbot is upgraded with `scripts/upgrade.sh` and the
plugins updated through the marketplace.

## Scope
- In: every mock screen (Home, Sessions + Chat, Files, Tasks, Routines, Agents, Games, Settings,
  the agent menu, the command ring, theme switch, attachments to ~/inbox, discard prompt), and every
  unticked BUILD-GAPS item except those listed under Out.
- Out (nothing in the mock needs them now): plugin `widgets` slot (no plugin offers a Home card
  since the gospel card was removed); multi-user anything.
- Must not: send the kernel token to the page; weaken `web/server/main.ts`'s security rules; write
  outside the vault from Files (above it is read-only, secrets under `~/.zenbot` unreadable);
  restart `zenbot` directly; edit applied migrations.

## Contract between kernel and web (new or changed, all under `/api`)
- `GET /tasks?done=7d` adds tasks ticked in the last 7 days (`done: true`, `done_on`). Every task
  has a stable `id` (its `🆔`, or a synthetic one for lines without it), `project`, `goals`,
  `agent` (name it was handed to, if any) and `session`.
- `POST /tasks` `{project, text, due?, priority?, goal?}` → the task, written with a new `🆔`.
- `PATCH /tasks/{id}` `{done?, dropped?, text?, due?, priority?, goal?, project?}`: edits the line in
  place in the Obsidian Tasks format (`✅ YYYY-MM-DD` on done, `[-]` dropped, a project change moves
  the line); 409 if the line changed since it was read. `DELETE /tasks/{id}`.
- `GET /tasks/meta` → `{projects: [{id, title}], goals: [{id, title, status}]}`.
- `POST /tasks/{id}/agent` `{agent?}` hands it to that agent (default zenbot).
- `GET /prefs`, `PATCH /prefs` (merge): `name`, `home_layout` (JSON), `default_model`,
  `default_effort`. New sessions use `default_model`/`default_effort` over `ZEN_DEFAULT_MODEL`.
- `GET /prompt-files/USER.md`, `PUT` `{text}`: backed up to `~/.zenbot/backups/prompt-files/`.
- `GET /agents`: each agent adds `busy` (`[{id, title}]`), `model`, `can_use` (a plain line),
  `chats`; each role adds `uses_30d`.
- `GET /version` adds `notes: [{version, date, items: [..]}]` from `CHANGES.md` (plain words).
- `GET /ideas` → three short new-chat ideas from the assist model (cached for the day).
- Engines: `GET /engines` (signed in, how), `POST /engines/{id}/login` → a link and code flow where
  the CLI allows it, else instructions; `POST /engines/{id}/logout`; OpenRouter key stored as a
  secret, never returned.
- Stop one subagent; `/board` carries each waiting session's open question; `PATCH /sessions/{id}
  {archived:false}` undoes an archive (verify each, add what's missing).

## Web server (`web/server`)
Files reads from `/` down, writes only inside the vault; `~/.zenbot` secrets (`token`, `env`,
`matrix.env`, `matrix/`, the database) are never served. Property facets and backlinks for vault
notes. Inbox listing (`~/inbox/<day>/`).

## Plugins
The host (`PluginFrame.svelte`) sends the app's theme (not the OS's), the new design tokens as MCP
Apps style variables, `displayMode` and `containerDimensions`, resent on change. zenread: the
reader of `*.md` in Files (properties as chips, tickable tasks, links, Linked from, stepping, Ask
zenbot, Edit → zenwrite). zenwrite: the Edit (Ctrl+S, back to reading). zensudoku: follows the
theme. zenvault: project status values, task-line lint rule.

## Assumptions (Ze's open decisions taken at my recommendation, 2026-11-19)
zenread reads, zenwrite edits, Files lists and previews; the app writes `tasks.md` (agents through
zenbot); Drop sits next to Delete; paused routines show dimmed with no warning; release notes come
from `CHANGES.md` written at release; no time-zone setting; Stop on a helper really stops it.

## Criteria
- `scripts/check.sh` passes locally and on CI for PR #24; each plugin's CI passes.
- e2e assertions cover each screen (`web/tests/zen-e2e.mjs`).
- Side by side, each real screen matches the mock's (desktop 1146×821 and phone 390×844, light
  and dark), no horizontal overflow, no page errors.
- `scripts/upgrade.sh` installs it and `zen status` is healthy; plugins show the new versions.
