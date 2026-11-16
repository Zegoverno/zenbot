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
