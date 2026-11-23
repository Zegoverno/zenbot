# zen web

zen in the browser, served from the kernel's machine and private behind a signing-in proxy:
everything the zen terminal app does (the board, sessions, the agent chat, files, tasks, agents),
with a minimal Markdown preview and plain text editor in Files.

## zen

The same screens as the terminal app, with its keys, and a mouse and a phone way to everything:

- **The board** (`/`): Sessions (channels, sessions, jobs, archived; waiting first; subagents
  under their session; another agent's session shows `@name`), Files (the vault, with the home folder one step up: read, and
  `n` new file, `Shift+N` new folder, `r` rename, asked inline, never over anything), Tasks (the vault's open tasks by project, most pressing first; hand one to an agent
  with `a a`) and Agents (each agent with what it may use, a broken `agent.toml` with its error;
  Enter opens its `SOUL.md`, `IDENTITY.md` and `agent.toml` to edit, save (`ctrl+s`; a refused
  change shows the kernel's reason and writes nothing), delete (asked first; never zenbot) or
  start a session with; `+` makes one, `n` starts a session with the chosen one). `1 2 3 4` or Tab
  switch views, `n` starts a session (or the `+ new session` button), `/` filters, `u` shows archived (your
  sessions), `Shift+U` or "Show job runs too" adds the job runs.
- **Keyboard only.** Everything has a key. One panel is always focused (thin accent outline; a
  click, `Ctrl+\` or opening a view moves it; `Ctrl+Shift+\` closes the side panel or brings back
  the last one). `Ctrl+.` opens the ring bottom right: its button shows the focused panel's icon and
  it fans out that panel's commands (up to six, plus Focus ⟷ when split); then a letter or `1`–`6`
  runs one (arrows and Enter too). `Ctrl+K` is the command list: fuzzy search over the panel's
  commands, the global ones, plugin commands, sessions and files, each with its shortcut;
  `Ctrl+E` on a row changes its key (a clash asks "Take it from …?"; kept in this browser as
  `zen.keys`). `?` lists the shortcuts that work now. `g` then `h s f t r a m p` goes to Home,
  Sessions, Files, Tasks, Routines, Agents, Games, Preview; `1`–`9` pick a tab; `c` a new chat; `@`
  the agent menu. Single letters work when you are not typing: in a chat `Esc` on an empty box
  leaves it (then `j`/`k` walk the steps, `r` renames, `x` archives, `i` or Enter writes), and `/`
  filter or search, `n` new, `e` edit, `r` rename or run, `x` archive, `a` ask zenbot, `j`/`k` or
  ↑↓ move, Enter open, Esc back are the same wherever they fit. Plugin UIs add their own commands
  (PLUGINS.md, "Keys and commands").
- **A session** (`/?s=<id>`): a new one starts as zenbot or the agent picked at its top
  or footer (or `/new @rex`); the chat streams the reply, folds tool calls into runs (`ctrl+o`),
  shows questions with clickable options and the next-prompt suggestion (Tab takes it). Files
  attach with 📎, a paste or a drop on the chat: each goes at once to `~/inbox/<YYYY-MM-DD>/`
  (up to 1 GiB, never overwriting: a clash gets `-2`), shows its progress, and the prompt names
  its path as `(attached ~/inbox/…)` so zenbot reads it with its own tools. Enter
  sends (also during a turn: it queues, shown above the box with a Steer button); unsent text stays
  with its session (in this browser), Esc interrupts,
  ↑↓ walk your prompts, `/` lists zen's
  commands (zenbot's own table; kernel commands run through the `zen` command line, so they answer
  as everywhere). `ctrl+b` opens the side panel with the same views, `shift+tab` gives it
  the keys. A link or a path in the chat opens there.
- **Files search**: Files opens in the vault (`ZEN_WEB_ROOT`; the `vault` and `home` buttons, the
  `~` crumb and `..` reach the home folder). The field at its top (`/` focuses it) searches as you
  type: file and folder names under the shown folder at once (`GET /api/fs/find`, which keeps home's
  rules: no secrets, dot entries or symlinks, bounded), and after a short pause (3+ characters) or
  Enter the kernel's `notes` search (`POST /api/zen/search`: words and meaning over wiki pages and
  every other vault note) with a snippet each, listed after the names. ↑↓ and Enter pick a result,
  a click opens it through the normal opener (plugins included), Esc clears. Without the kernel's
  answer the name matches still show.
- **Preview** (side panel): any dev server on this machine, at a screen size. The tab lists what
  listens on ports 3000–9999 (`GET /api/preview/servers`, `server/servers.ts`: read from `/proc`,
  only the owner's own processes, never system services or zen's own ports), named by app (its
  package.json name, git folder, user unit or folder, not the port), with ↻ restart and ✕ stop for
  each (`POST /api/preview/servers/{stop,restart}`: the port is looked up again, never a pid from the
  page; a service unit goes through `systemctl --user`, any other process is stopped by signal and
  started again as a transient `zen-preview-<port>-…` user unit, so it outlives zen web's own restarts).
  `a` opens the list; a port can still be typed. It shows the app, at a screen size,
  under zen's own address: `/preview/<port>/…` (`server/preview.ts`) forwards to `localhost:<port>`
  (hot reload included), adds `/zen-preview.js` to its pages and drops frame-blocking headers, so it
  works wherever zen does (one port, no other sign-in, nothing a firewall drops). An app started with
  the base `/preview/<port>/` (Vite: `--base /preview/5173/`) gets paths as they are; a static page
  gets them without the prefix (learnt per port from its first page). With Point on, a click pins the
  element (source file and line from Svelte's or React 18's dev build, or a `data-source`
  attribute; box, key styles, console errors, a screenshot) to the chat as a chip, numbered #1, #2…
  for the comment. The screenshot is taken on this machine (`server/shot.ts`: Playwright's headless
  Chromium, or `ZEN_WEB_CHROMIUM`, opens the same page at the owner's screen size), so nothing is
  uploaded from the browser; it's a fresh load, without an open menu or typed text. If it fails, the
  browser's own capture is uploaded instead. **The previewed app runs on zen's origin** (the owner's choice): its code, its
  packages included, can act as zen's page, so preview only apps you trust. zen's own ports are
  refused, and the proxy's login headers are stripped before forwarding.
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
`ZEN_WEB_DEV_HOSTS` (host names Vite's dev server accepts besides localhost), `ZEN_WEB_PREVIEW_SYSTEM`
(`1` also lists processes in `/system.slice/`; tests only).

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
A download (`GET /api/fs/download?path=`, the Download button or `d`) follows the same read rules (no
secrets, regular files only) and is streamed as `application/octet-stream` with `attachment`,
`nosniff` and a sandboxing CSP, so nothing downloaded is ever rendered by this origin.
Any signed-in user can stop or restart the owner's own dev servers from Preview (processes of the
server's user on ports 3000–9999); system services (cgroup `/system.slice/`), zen's own ports and
the web server's own unit are never listed, and a port that isn't listed is a 404.
