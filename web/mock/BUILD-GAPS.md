# What the redesign needs outside the UI: zenbot and its plugins

The open list for the major version: what the mock shows that zenbot (kernel `crates/zend`, web
server `web/server`, shell `web/src/zen`) or a plugin (zenvault, zenread, zenwrite, zensudoku) can't serve today. "Have" is what already exists. Keep it up to date as the mock
changes; tick each item as it ships. "Verify" = not checked in the code yet.

## 1. zenbot kernel and server

### Tasks
- [ ] **Write tasks** (have: read-only `GET /api/tasks`, open tasks only). Tick (`[x]` + `✅` date),
      untick, add (new `🆔`, project, due, priority, goal link), edit text/due/priority/goal/project
      (a project move = move the line to the other `tasks.md`), drop (`[-]`), delete. Write the line
      in place in the Obsidian Tasks format; refuse when the file changed since it was read.
- [ ] **What the editor lists:** projects (folder + title) and active goals (`Goals/*.md`).
- [ ] **Recently done tasks** with their `✅` date, for the Done group and Home's task cards
      (ticked today stays struck through until tomorrow), e.g. `?done=7d`.
- [ ] **Hand to a chosen agent** (have: `POST /api/tasks/{id}/agent` → `task_agents` keeps the
      session id; always the default agent). Add `{ agent }`; return the agent's name with the task.
- [ ] **Tasks without `🆔`** can't be handed: give them an id on write or when handed.

### Files
- [ ] **Property facets:** values with counts for `type`, `status`, `tags` under a folder, and the
      filtered list (OR inside a property, AND across, tags any/all). Have: name-only `fs/find`.
- [ ] **Backlinks** ("Linked from") for a note: an endpoint over the search index.
- [ ] **Access rule as the mock shows it:** read and write inside the vault (`~/.zenbot/global`),
      read-only above it, up to `/`. The web server's Files view (`web/server/home.ts`) today stays
      inside home and its only writes are create, mkdir and rename: widen it to match.
- Have: list/read/write (`PUT /api/file`), create, mkdir, rename, `[[link]]` resolution (`fs/locate`).

### Home
- [ ] **Layout storage** (Customize: which cards, in what order, sizes, and each task card's view:
      show, order, project, goal, priorities, rows), one per instance, server-side.
- [ ] **Owner's name** (Settings › You, Ze 2026-11-19): `GET/PUT /api/me {name}`, stored in
      the kernel's config. zenbot sets it in the first chat while it fills `USER.md`; the owner can
      change it in Settings. Used by Home's greeting and the agent menu (avatar letter, name).
- [ ] **Needs you:** each waiting session's open question in one call (verify whether `/board` serves it).
- [ ] **Home cards from plugins:** a new `widgets` slot (today `contributes` allows only `views`,
      `openers`, `games`, `commands`; `widgets` is rejected): id, title, `ui://` URI, size
      (small/wide), refresh. Routines' card reads jobs' last results (have: `/jobs/runs`).
- [ ] **New-chat ideas:** a cheap-model (S1) endpoint for the 3 suggestions on an empty chat
      (`/suggestions` today is S1 stats, not this).

### Chat and sessions
- [ ] **Stop one subagent** from its panel (have: stop the whole turn).
- [ ] **Live subagents and their current step** in one shape the panel draws (verify `child_*` events).
- [ ] **Undo archive** = `archived: false` (verify PATCH accepts it and clears the outcome).
- Have: rename, archive with outcome, Part done → continue (`continues`), per-session agent,
  task ↔ session link. UI only: follow-bottom scroll, answer in place, close ×, panel tabs.

### Agents
- [ ] **Per-agent summary in `GET /api/agents`:** what it's doing now (busy sessions), its model,
      a plain "can use" line from `agent.toml` limits, and its chat count (have: count only in the
      detail call).
- [ ] **Role usage:** how often each role ran in the last 30 days (`sessions.role` already records it).
- Have: roles per agent, read-only (`roles` on each agent). Changing a role = ask the agent (its file).

### Settings
- Mock shows only what exists (models, connections, routines, plugins, memory, updates via the
  kernel API). Proposed additions wait for Ze's call before they're listed here.
- [ ] **Engine sign-in/out from the app** (Settings › Models, Ze 2026-11-19): drive `claude` and
      `codex` login flows (link + code) from the kernel, an OpenRouter key field (stored as a
      secret, never shown back); sign-out confirms first. Have: `authenticated` per engine.
- [ ] **Default model and effort from the app** (Ze 2026-11-19): a runtime setting that overrides
      `ZEN_DEFAULT_MODEL` without a restart; only models of signed-in engines offered.
- Routines moved to their own top tab (no kernel change: `/api/jobs` as before).
- Plugin **Update to x.y** uses the existing install route with the marketplace's newer version.
- [ ] **Release notes in plain words** (Settings › Updates, Ze 2026-11-19): a short "what's new"
      written with each release (e.g. a `CHANGES.md` section per version the kernel serves with
      `/api/version`), shown for an update waiting and for the running version. Have: commit subjects.
- Failed-routine warning (Routines badge, banner, Home line; Ze 2026-11-19): no kernel change,
  `GET /api/jobs` has `last_status`, `last_error` and `failures`; "See the run" uses `/jobs/runs?job=`.
- [ ] **Edit USER.md from the app** (Settings › You, Ze 2026-11-19): read and write
      `~/.zenbot/USER.md`, which sits above the vault where Files is read-only. A narrow route
      (or an allow-list entry for this one file), keeping the prompt-file backup on every save.

### Plugin host (shell, `PluginFrame.svelte`)
- [ ] **Theme from the app, not the OS:** the mock has its own auto/dark/light switch; the host
      context sends only `prefers-color-scheme` today. Send the app's choice and resend on change.
- [ ] **New design tokens** (paper, soft, faint, accent, radius, serif title font) mapped to the
      MCP Apps style variables, so plugins look native.
- [ ] **Where a plugin sits:** tell it full page vs side panel vs phone (`displayMode`,
      `containerDimensions`), resent when the split changes; the shell draws the × and the title,
      so plugins drop their own.
- [ ] **"Ask zenbot" from a plugin:** a note's path into the chat beside it. Have: `ui/message`
      queues a draft only if a chat is open; make it open a new chat when none is.

## 2. zenvault (the vault method)
- [ ] **Project status values:** the method says a project note has `status` but lists no values
      and nothing checks them. Use active, done, dropped (as goals); add a lint rule.
- [ ] **Task lines written by the app pass lint:** `🆔` on every task, `✅` dates, `[-]` dropped,
      only 🔺 ⏫ 🔽 ⏬ offered (🔼 read). Add a task-line rule if there is none (verify).
- [ ] **Allowed property values as data** (wiki `type`s and statuses, goal statuses), so Files'
      filter can label and order them instead of guessing.
- [ ] **Who owns the task format:** the kernel parses `tasks.md` (`tasks.rs`); zenvault defines it.
      Recommendation: the kernel keeps reading and writing; zenvault's method stays the spec and
      its lint the check, so the rules live in one place.

## 3. zenread (reader)
- [ ] **Replace the built-in reader** in Files (decided: the mock's reader is a stand-in): opener
      for `*.md` in read mode, with the mock's features: properties as chips, tickable tasks,
      `[[links]]`, Linked from, ‹ n of N › stepping (it already has "next note in the folder"),
      Ask zenbot with the path, Edit hand-off to zenwrite.
- [ ] Follow the host's theme, tokens and container size (see 1, plugin host).

## 4. zenwrite (editor)
- [ ] **Be the Edit in Files:** opener in edit mode replacing the mock's plain-text editor; Ctrl+S,
      back to the reader when done.
- [ ] Follow the host's theme, tokens and container size.

## 5. zensudoku (Games)
- [ ] Follow the host's theme, tokens and container size; nothing else needed (it already
      contributes a `games` entry).

## Decisions taken in the mock
- One user per zenbot instance: no owners, no multi-user rules.
- Files' reader/editor is a stand-in; reading moves to zenread, editing to zenwrite.
