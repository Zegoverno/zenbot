# Building a zenbot plugin

A zenbot plugin is a small app that zenbot can install and show in its own UI. It is built on open
standards, so the same plugin also runs on its own (as a web app) and inside other MCP hosts such
as Claude and ChatGPT:

- an **MCP server** (stdio) with the plugin's tools;
- optionally, one or more **UIs** following [MCP Apps](https://modelcontextprotocol.io/docs/extensions/apps)
  (`ui://` HTML resources);
- optionally, **skills** (folders with a `SKILL.md`);
- a manifest, **`plugin.json`**.

Only `plugin.json`'s `contributes` and `kernel` keys are zenbot's own. Everything else is the standard.

The design behind this lives in [ADR 0001](docs/adr/0001-plugins.md). The first plugins are useful
references: [zenread](https://github.com/Zegoverno/zenread), [zenwrite](https://github.com/Zegoverno/zenwrite)
and [zensudoku](https://github.com/Zegoverno/zensudoku).

## The manifest: `plugin.json`

```json
{
  "name": "zenread",
  "version": "0.1.0",
  "title": "zenread",
  "description": "A calm Markdown reader.",
  "author": "Your Name",
  "license": "MIT",
  "repository": "https://github.com/you/zenread",
  "server": { "command": "node", "args": ["mcp/dist/server.js"] },
  "contributes": {
    "views":    [{ "id": "library", "title": "Reading", "ui": "ui://zenread/app" }],
    "openers":  [{ "id": "read", "title": "Read", "ui": "ui://zenread/app", "patterns": ["*.md"], "mode": "read" }],
    "games":    [],
    "commands": [{ "name": "read", "tool": "zenread_open", "args": "<path>", "description": "Open a note in the reader." }]
  },
  "skills": "skills/",
  "kernel": ["files:read"]
}
```

- `name`: `[a-z0-9-]{2,40}`, unique in the marketplace. Every `ui` is `ui://<name>/…`, and every
  tool name should start with `<name>_`.
- `version`: semver. The release tag is `v<version>`.
- `server.command`: `node` only, for now. The server must be **one self-contained file**: bundle
  your dependencies. The installed plugin has no `node_modules`, and zenbot never runs `npm` for
  you.

### Slots (`contributes`)

zenbot draws the shell. A plugin fills these slots:

| Slot | Where it shows | Fields |
|---|---|---|
| `views` | a tab on the board and in the side panel, after Sessions, Files, Tasks and Agents | `id`, `title`, `ui` |
| `openers` | "open with" for matching files in Files | `id`, `title`, `ui`, `patterns` (globs on the file name), `mode` (`read` or `edit`) |
| `games` | an entry in the **Games** tab's game picker | `id`, `title`, `ui`, `description` |
| `commands` | a slash command in the web chat and in Matrix, and `zen <plugin> …` | `name`, `tool`, `args`, `description` |

When two plugins claim the same opener, the owner picks the default in **Settings → Plugins**.
An install never takes over a slot silently: the built-in reader and editor stay the default until
the owner chooses.

### Permissions (`kernel`)

The plugin's server runs in a sandbox (bubblewrap). It can read its own folder and write
`$ZEN_PLUGIN_DATA`. It reaches nothing else unless the manifest asks:

| Permission | Grants |
|---|---|
| `files:read` | read the owner's files under `$ZEN_PLUGIN_FILES` |
| `files:write` | read and write them (`files:write:<subfolder>` narrows writing to that subfolder) |
| `network` | outbound internet. Without it, the server has no network at all |
| `ai` | ask the host's model for completions (MCP sampling) |

zenbot shows these permissions at install and asks the owner to approve them. An upgrade that asks
for more is approved again.

## The server

**Current limitation:** plugin AI sampling is disabled in zenbot until the CLI engines support
provider-native output-token limits and per-plugin budgets. No provider completion starts; AI
tools return an unavailable error. Editing, reading, games and normal chat are unaffected.

- stdio MCP, any revision from 2024-11-05 to 2025-06-18. The TypeScript SDK
  (`@modelcontextprotocol/sdk`) and `@modelcontextprotocol/ext-apps/server` (`registerAppTool`,
  `registerAppResource`) are the easy path.
- Environment: `ZEN_PLUGIN_FILES` (absolute root of the files you may touch) and `ZEN_PLUGIN_DATA`
  (your writable folder). Every path a tool takes is relative to `ZEN_PLUGIN_FILES`: refuse `..`,
  absolute paths, links that point out, and dot folders. When `ZEN_PLUGIN_FILES` is unset (in
  Claude Desktop, say), file tools must say so and refuse.
- AI: ask the host with `sampling/createMessage`. When the host can't sample, return a clear "AI
  isn't available here" result instead of failing.

## The UI

- Use `App` from `@modelcontextprotocol/ext-apps`. Never hand-roll postMessage.
- It runs in a sandboxed iframe with an opaque origin: no cookies, no storage you can count on,
  no network unless the resource's `_meta.ui.csp` declares the domains. Build it as one HTML file
  (for example with `vite-plugin-singlefile`).
- Call your own tools with `app.callServerTool(...)`; zenbot routes them through its kernel,
  which checks your permissions and records each call.
- zenbot adds its own context to the host context under `zen`:
  `{ "pane": "board" | "side", "path": "<relative path or null>", "mode": "read" | "edit", "slot": "view" | "opener" | "game" }`.
  Read it with `app.getHostContext()` / `onhostcontextchanged`, and cope with its absence.
- Open another file: `app.openLink({ url: "zen://open?path=<urlencoded relative path>" })`. zenbot
  sends it to that file's opener. Plugins never talk to each other directly.
- Follow the host's `theme` (light or dark) and its CSS variables.
- `app.updateModelContext(...)` tells the chat what's on screen. `app.sendMessage(...)` posts into the
  chat when the user asks for help.

## Releasing

A release is a **GitHub release** tagged `v<version>` with one asset,
`<name>-<version>.tgz`: a gzipped tar of the plugin as installed. It holds:

```
plugin.json
mcp/dist/server.js        (self-contained)
<your built ui files>     (whatever plugin.json and the server read at run time)
skills/                   (optional)
README.md  LICENSE
```

The archive must not contain links, absolute paths, `..`, `node_modules` or dot files. zenbot
downloads it, checks its sha256 against the marketplace entry, unpacks it and runs it. Nothing is
built or installed on the owner's machine.

Try it locally before releasing: `zen plugins add ./my-plugin-0.1.0.tgz` (or a folder holding the
built plugin), then open zenbot's web app.

## Listing in the marketplace

The marketplace is [`marketplace/plugins.json`](marketplace/plugins.json) in this repo. The zenbot
web app reads it (**Settings → Marketplace**) and installs from it in one click, and it is published
at the [marketplace page](https://zegoverno.github.io/zenbot/).

1. Open a **"List a plugin"** issue (the form asks for everything below), or open a pull request
   adding your entry to `marketplace/plugins.json` yourself:

   ```json
   {
     "name": "zenread",
     "title": "zenread",
     "description": "A calm Markdown reader.",
     "author": "Your Name",
     "repository": "https://github.com/you/zenread",
     "license": "MIT",
     "version": "0.1.0",
     "archive": "https://github.com/you/zenread/releases/download/v0.1.0/zenread-0.1.0.tgz",
     "sha256": "<sha256 of the archive>",
     "kernel": ["files:read"],
     "contributes": ["views", "openers", "commands"],
     "categories": ["reading"]
   }
   ```

2. CI checks the entry (`scripts/marketplace-check.mjs`): it downloads the archive, checks the
   sha256 and the archive's rules, and checks that its `plugin.json` matches the entry (name,
   version, permissions, slots).
3. A maintainer reviews the code at that tag and approves. Nothing is listed without that approval.
   Upgrades are pull requests that change `version`, `archive` and `sha256`, and they are reviewed
   the same way.

Requirements for a listing: an open-source licence, a public repo, a README that says what the
plugin does and why it needs each permission, and no telemetry or network calls beyond those the
README declares.
