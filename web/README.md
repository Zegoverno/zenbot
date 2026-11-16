# zen web

zen in the browser, served from the kernel's machine and private behind a signing-in proxy:
everything the zen terminal app does (the board, sessions, the agent chat, files, tasks, agents),
with a minimal Markdown preview and plain text editor in Files.

## zen

The same screens as the terminal app, with its keys, and a mouse and a phone way to everything:

- **The board** (`/`): Sessions (channels, sessions, jobs, archived; waiting first; subagents
  under their session; another agent's session shows `@name`), Files (the home folder: read, and
  `a` new file, `A` new folder, `R` rename, asked inline, never over anything), Tasks (the vault's open tasks by project, most pressing first; hand one to an agent
  with `a a`) and Agents (each agent with what it may use, a broken `agent.toml` with its error;
  Enter opens its `SOUL.md`, `IDENTITY.md` and `agent.toml` to edit, save (`ctrl+s`; a refused
  change shows the kernel's reason and writes nothing), delete (asked first; never zenbot) or
  start a session with; `+` makes one, `n` starts a session with the chosen one). `1 2 3 4` or Tab
  switch views, `n` starts a session (or the `+ new session` button), `/` filters, `a` shows archived, `J`
  shows archived jobs too (hidden by default; kept in this browser).
- **A session** (`/?s=<id>`): a new one starts as zenbot or the agent picked at its top
  or footer (or `/new @rex`); the chat streams the reply, folds tool calls into runs (`ctrl+o`),
  shows questions with clickable options and the next-prompt suggestion (Tab takes it). Files
  attach with 📎, a paste or a drop on the chat: each goes at once to `~/inbox/<YYYY-MM-DD>/`
  (up to 1 GiB, never overwriting: a clash gets `-2`), shows its progress, and the prompt names
  its path as `(attached ~/inbox/…)` so zenbot reads it with its own tools. Enter
  sends (also during a turn: it queues), Esc interrupts, ↑↓ walk your prompts, `/` lists zen's
  commands (zenbot's own table; kernel commands run through the `zen` command line, so they answer
  as everywhere). `ctrl+b` opens the side panel with the same views, `shift+tab` gives it
  the keys. A link or a path in the chat opens there.
- **Files fallback** renders Markdown safely using text nodes, and previews text/code verbatim.
  Vault Markdown has Edit (`e`), a plain textarea, explicit Save (`Ctrl+S`) and Read
  (`Alt+W`). Saves are atomic with an optimistic hash; conflicts retain your local edits.
  Unsaved edits ask before switching to Read. Rich readers/writers are optional file openers,
  not standalone core tabs. Removing every plugin leaves this fallback available.

How: the server relays the zen kernel (`server/zen.ts`: the routes the terminal app uses, and the
session WebSocket, with the token added on the server, so it never reaches the page; a socket
opens only for pages of this site), shows the home folder (`server/home.ts`: never
zen's token, settings or database, the engines' sign-ins, ssh, gpg or cloud credentials, nor
anything outside home; it only creates empty files and folders and renames, never over anything), and runs kernel commands (`server/commands.ts`: listed ones only, plain
arguments, no shell). The page is `src/zen/`.

## Plugin host

**Settings → Plugins** lists installed versions, approved permissions, enable/disable and removal
(with confirmation; plugin data stays). **Marketplace** offers Install/Update. The kernel checks
release archives; its exact permission request is shown before approval, including renewed approval
if the requested permissions change while the dialog is open. Marketplace configuration belongs to
the kernel (`ZEN_MARKETPLACE`); see [PLUGINS.md](../PLUGINS.md) and [ADR 0001](../docs/adr/0001-plugins.md).

Enabled plugins add board and side-panel view tabs. **Games** lists their installed games and
remembers the last played in this browser; with none it links to the marketplace. Vault files have
**Open with…** choices for read/edit, plus per-pattern defaults in Settings. Built-ins stay the default
until explicitly changed; files outside the vault always use the built-ins.

MCP Apps run through `AppBridge` in opaque-origin iframes (`allow-scripts allow-forms`, never
`allow-same-origin`). A host CSP precedes all plugin HTML, allowing no network by default and only
validated declared CSP domains otherwise. No kernel token, host cookies, popups or top navigation
are granted. Tool calls pass through the authenticated server relay to the kernel, which enforces
plugin/tool ownership, visibility, sandbox permissions and recording. `zen://open` links route
vault-relative paths through the pane's opener; http(s) links open a separate tab. Host context
includes theme, styles and `{ zen: { pane, path, mode, slot } }`.

Plugin slash commands come from the shared command table (refreshed when plugin state changes).
Results with a UI resource open in the side panel; otherwise their text appears in chat. The tool
result must expose `_meta.ui.resourceUri` (or the command table supply `ui`) for this host to locate
it. Model-context updates are held per session and prepended to its next prompt once, as a labelled
`app_context` block (the kernel has no separate context field yet). `sendMessage` only queues a
visible DRAFT attributed to the plugin ID, even for zero-permission games. It never submits a
prompt: the owner must click Send plugin draft (or Dismiss). The owner's composer stays separate;
plugin text is tagged so it cannot execute as a slash command. At most 20 drafts queue per chat.

The browser tests use a stand-in kernel and the real MCP Apps SDK, covering permissions, views,
games, openers, commands, context and iframe isolation/CSP. They do not prove live archive installs,
bubblewrap/network isolation, sampling or real plugin-server interoperability; those require the
kernel's integration tests and a live development kernel. The plugin files root is assumed to be
the web vault; deployments overriding `ZEN_PLUGIN_FILES` must keep those aligned.

## Markdown files

`?path=wiki/Note.md` opens a vault note for editing in Files; `?notes` opens Files at the
vault; `?new=wiki` asks a filename and title, then opens an unsaved draft. Save creates it
only if no file has appeared in the meantime. Core never rewrites Markdown metadata,
alternatives or changelogs; they remain ordinary source text. Versions, AI checks, shrinking,
notes finding and rich editing belong to the writer plugin, not core routes or hidden panels.

## Run

```bash
npm ci
ZEN_WEB_PORT=4401 npm run dev   # the server with Vite's hot reload (the installed app uses 4400)
npm run check            # types + unit tests
node tests/e2e.mjs       # minimal Files: the real server and headless Chromium on a scratch folder
node tests/zen-e2e.mjs   # the zen app: the same, with a stand-in kernel (HTTP and WebSocket)
```

zenbot's `scripts/upgrade.sh` builds this folder with the kernel and `scripts/web-install.sh`
runs the build as the user unit `zen-web` on 127.0.0.1:4400, from
`~/.local/share/zen-web/current`, so work in the checkout never touches it.

Settings (environment): `ZEN_WEB_ROOT` (the vault, default `~/.zenbot/global`), `ZEN_WEB_HOME`
(the Files view's home, default `$HOME`), `ZEN_BIN` (default `~/.zenbot/bin/zen`), `ZEN_WEB_PORT`
(4400), `ZEN_URL`, `ZEN_TOKEN_FILE`, `ZEN_WEB_AUTH_HEADER` (the proxy's login header, default exe.dev's
`X-ExeDev-UserID`), `ZEN_WEB_AUTH` (`off` drops the login-header check; local tests only),
`ZEN_WEB_DEV_HOSTS` (host names Vite's dev server accepts besides localhost).

Security: loopback only; every request needs the proxy's login header; writes need `X-Zen-Web: 1`
(no cross-site writes); paths stay inside the root (Markdown only, no dot folders, no symlinks out);
one write at a time per note, through a temporary file and a rename; an attachment (`POST /api/upload?name=`)
keeps only a plain last segment of its name, opens its file exclusively in the inbox's day folder
(not a symlink out), streams under the size cap and removes a partial file on failure. The home folder's
writes (`POST /api/fs/create`, `mkdir`, `rename`) take one plain name (no `/`, `\`, NUL, `.` or `..`, at most
255 bytes) that is not a secret's (no new `.env` or `id_rsa`), in a folder inside home; they never overwrite
(a taken name is a 409), a rename stays in its folder, and home, `~/.zenbot` and what is within two levels of
it (`~/.zenbot/global`, `~/.zenbot/agents/zenbot`) can't be renamed. nginx in
front must allow the size on that route (`client_max_body_size 1g`, request buffering off).
