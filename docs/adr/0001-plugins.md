# ADR 0001: Plugins are MCP servers with an optional MCP App UI

Status: proposed (2026-11-14)

## Context

zenbot keeps the reader, the writer and Sudoku inside its own apps (the TUI and the separate web
app). These three should become products of their own: a hosted web app, a desktop app and a
mobile app, each usable without zenbot. zenbot should also be able to install them and show them in
its own UI. Other pieces (a richer Tasks screen, an agents org chart, an Obsidian-like vault) should
plug in the same way.

## Decision

A **plugin** is a git repository containing:

- an **MCP server** with the plugin's tools;
- optionally, one or more **MCP App UIs** (`ui://` HTML resources, the
  [MCP Apps](https://blog.modelcontextprotocol.io/posts/2026-01-26-mcp-apps/) standard). The same
  bundle is the standalone app;
- **skills** and **jobs**;
- a manifest, `plugin.json`.

Because the tools and the UI follow the standard, a plugin also works inside Claude and ChatGPT,
as an inline or full-screen widget. Only `contributes` is zenbot's own.

```json
{
  "name": "reader",
  "version": "0.1.0",
  "server": { "command": "node", "args": ["server/index.js"] },
  "contributes": {
    "views":    [{ "id": "library", "title": "Reading", "ui": "ui://reader/library" }],
    "openers":  [{ "id": "read", "ui": "ui://reader/app", "patterns": ["*.md", "*.pdf", "*.epub"], "mode": "read" }],
    "commands": [{ "name": "read", "tool": "reader_open", "args": "<url|path>" }]
  },
  "skills": "skills/",
  "jobs": [],
  "kernel": ["files:read", "files:write:~/.zenbot/global/Reading"]
}
```

### Slots (zenbot's contribution points)

zenbot's shell owns the layout. Plugins fill these fixed slots and never draw the shell.

| Slot | Shown as | Built-in default |
|---|---|---|
| `views` | a tab on the board and in the side panel | Files, Tasks, Agents (plain) |
| `openers` | "open with" for a file type, inside the Files pane | a plain Markdown reader and editor |
| `widgets` | a tool result inline in chat (plain MCP Apps) | text |
| `commands` | slash commands in the shared command table (CLI, web chat, Matrix) | core commands |

When two plugins claim the same opener or view, the owner chooses the default in settings. An
install never silently takes a slot over.

### Host

- The web shell loads each `ui://` resource into a **sandboxed iframe**:
  - an opaque origin, with no cookies and no kernel token;
  - a CSP that allows only the domains the resource declares.
- The iframe and the shell talk by **postMessage JSON-RPC**, as MCP Apps defines it.
- A UI's `tools/call` goes **through the kernel**, which checks the plugin's permissions and
  records the call on the tape, exactly like an agent's tool call.
- On top of the standard host context (theme, size, display mode), zenbot adds `zen/context`:
  - the pane the UI is in (board or side panel);
  - the open file or session.
- Plugins never call each other directly. A link to a file asks the shell to open it
  (`zen/open {path}`), and the shell sends it to the chosen opener.

### Install and isolation

- `zen plugins add <git url>@<tag>` pins a version under `~/.zenbot/plugins/<name>/<version>/`.
- It shows the requested `kernel` permissions. The owner approves them once, and approves again
  whenever an upgrade asks for more.
- The plugin's server runs in the same bubblewrap and pasta sandbox as non-zenbot agents. It can
  reach its own folder, a data folder, and only the vault paths it was granted.
- `zen plugins rm` stops the server and moves the plugin to backups. Its data stays.

### Core or plugin

The rule:
- **Core** is whatever zenbot needs to run as an agent, plus what every plugin relies on.
- **Plugins** are separate products, or richer screens over core data. Uninstalling one never
  breaks zenbot.

Core:
- kernel and engine;
- sessions and chat, with slash commands;
- Matrix, the default channel;
- the inbox (decisions waiting on the owner);
- the data and API for files, tasks and agents;
- the plain default screens for files, tasks and agents;
- settings and the plugin manager.

Plugins:
- now: `reader`, `writer`, `sudoku`;
- later, when wanted: `tasks` (kanban, goal tree, calendar), `agents-org`, `vault`.

The data behind Tasks and Agents stays in the kernel, because the harness itself uses it. A plugin
only replaces the screen.

## Consequences

- One plugin format serves zenbot, Claude, ChatGPT and the standalone apps.
- zenbot must implement an MCP Apps host (iframe, postMessage bridge, routing of tool calls through
  the kernel) and the `contributes` slots.
- Plugin servers run sandboxed, so a plugin that needs more (the network, other folders) has to
  say so in its manifest.
- Slots are added only when a real plugin needs one. `views`, `openers` and `commands` come first.
  `widgets` come with the first plugin that needs them.

## Host API (v0)

The precise contract between the kernel, the web shell and plugins. The plugin side (manifest,
slots, permissions, UI rules, release archive, marketplace entry) is [PLUGINS.md](../../PLUGINS.md);
this section is the host side.

**Install without running anyone's code.** A plugin is installed from a release archive
(`<name>-<version>.tgz`) whose sha256 the marketplace pins, the way Obsidian and VS Code install
extensions: zenbot downloads, checks and unpacks it; it never runs `npm` or a build. A local folder
or archive can be added too, for development.

**Kernel** (`crates/zend/src/plugins.rs`, uses `mcp.rs` for the server):
- Installed plugins live in `~/.zenbot/plugins/<name>/` with their data in
  `~/.zenbot/plugin-data/<name>/`; the list, versions, approved permissions, enabled flag and slot
  defaults in the database (new migration).
- The marketplace is a JSON file at a URL (`ZEN_MARKETPLACE`, default this repo's
  `marketplace/plugins.json` on `main`), fetched with a timeout and a size cap and cached briefly.
- `zen plugins` lists; `zen plugins add <name | path.tgz | folder> [--yes]` installs or upgrades:
  for a name, the marketplace entry's archive, checked against its sha256; the archive is unpacked
  into a staging folder refusing links, absolute paths, `..`, devices and anything over 50 MB; then
  `plugin.json` is validated (name `[a-z0-9-]{2,40}` and equal to the entry's, every `ui` a
  `ui://<name>/…` URI, known slots and permissions, `server.command` `node`), the requested
  permissions are shown and approved (unless `--yes`), and the folder is moved in place (the old
  version to backups). More permissions on an upgrade ask again. `zen plugins rm <name>` stops it
  and moves its folder to backups (data stays). Same commands in the shared table (`/plugins`).
- The server runs as an MCP stdio server named `plugin-<name>`, in bubblewrap (`sandbox.rs`): its
  plugin folder read-only, its data folder read-write, the files root (`ZEN_PLUGIN_FILES`, the vault
  `~/.zenbot/global`) read-only with `files:read`, read-write with `files:write`
  (`files:write:<sub>`: only that subfolder writable), no other home paths; network only with
  `network` (through pasta, as agents), otherwise none. Env `ZEN_PLUGIN_FILES`, `ZEN_PLUGIN_DATA`.
  Without bubblewrap the plugin doesn't run (an error, never unsandboxed). Agents see its tools
  through `find_tools`/`call_tool` like any MCP server.
- **Sampling:** currently fails closed, even with `ai`: the Claude/Codex CLI adapters have no
  verified provider-native output cap. Refusals are audited; no provider request starts. Re-enabling
  requires a native cap plus per-plugin request/spend budgets. Normal chat is unchanged.
- HTTP (owner token, like the rest of the API):
  - `GET /api/plugins` → `{ plugins: [{ name, version, title, description, contributes, permissions, enabled }], defaults: { "<pattern>:<mode>": "<plugin>/<opener id>" | "builtin" } }`.
  - `PUT /api/plugins/defaults` `{ key, value }`; `PATCH /api/plugins/{name}` `{ enabled }`.
  - `GET /api/plugins/marketplace` → the entries, each with `installed` (version or null).
  - `POST /api/plugins/install` `{ name, approve? }`: without `approve`, `{ needs_approval: true, permissions }`
    (nothing installed); with `approve: true` (and the same permissions), installs and returns the plugin.
  - `DELETE /api/plugins/{name}`.
  - `GET /api/plugins/{name}/resource?uri=ui://…` → `{ uri, mimeType, text, _meta }` (only `ui://<name>/…`).
  - `POST /api/plugins/{name}/call` `{ tool, arguments, session? }` → the MCP `CallToolResult`.
    Recorded like an agent's MCP call (caller `ui`), with the same timeout, cap and masking. Only
    that plugin's tools; tools whose `_meta.ui.visibility` excludes `app` are refused.
  - `zen --commands` includes each enabled plugin's `contributes.commands` (name, args,
    description, `plugin`, `tool`).

**Web shell** (`web/`):
- The server relays `/api/plugins*` to the kernel (token added server-side, writes need the write
  header).
- **Settings** (a page off the top bar): *Plugins* (installed: version, permissions, enable or
  disable, remove; the default opener per pattern and mode) and *Marketplace* (the entries, with
  Install / Update, a permission dialog before installing).
- Views: each enabled plugin's `views` become tabs after the built-in ones, on the board and in
  the side panel. **Games** is a built-in tab: a picker over every enabled plugin's `games`
  (the last one played opens), with a link to the marketplace when there are none. Openers:
  opening a file picks the default for its pattern and mode (`builtin` until the owner chooses);
  "Open with" lists the alternatives and can set the default.
- Each `ui://` resource renders in an `<iframe sandbox="allow-scripts allow-forms">` (no
  `allow-same-origin`: an opaque origin, no cookies, no token) through `AppBridge` from
  `@modelcontextprotocol/ext-apps/app-bridge`, with a CSP built from the resource's `_meta.ui.csp`
  (default: no network). Tool calls go to `POST /api/plugins/{name}/call`; `openLink` with
  `zen://open` opens the path in the same pane through its opener; http(s) links open a new tab;
  `sendMessage` posts into the open session's chat; `updateModelContext` is kept and sent as
  context with that session's next prompt.
- Slash commands from plugins call the tool and show its UI in the side panel (or the result text
  when it has none). In Matrix they return the result text.
