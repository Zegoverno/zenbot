# zen web

zen in the browser, served from the kernel's machine and private behind a signing-in proxy:
everything the zen terminal app does (the board, sessions, the agent chat, files, tasks, agents),
plus the writer, a calm writing page for Markdown notes.

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
- **The file viewer** opens in place of the Files tree: prose laid out for reading (width `< >`,
  spacing `:`, bionic `b`, focus `f` by paragraph or line with five dim levels `d`, play `p` word by
  word), links followed (`l`, Enter, ← back), headings `[ ]`, the folder's next / previous file (`n` `N`), the changelog apart (`h`), the front
  matter folded (`i`), its web link (`link`, `url`, `source` first) opened in a new tab with `Alt+O` or ↗ source. A vault note is written in the same pane (`e` or Write: the writer)
  and read again with `Alt+W` (or Read). Its settings are `~/.zenbot/reader.json`, shared with the
  terminal app.

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

## The writer

The writer is the file viewer's writing side, on the board or in the side panel beside a chat.
`?path=wiki/Note.md` (the zen terminal app's `e`) lands on the board writing that note; `?notes` on
Files at the vault; `?new=wiki` asks a file name, then its H1, then writes the new note there.

- **Files are plain Markdown.** Obsidian, git and every other tool see a normal note. Alternatives,
  ghosts and zenbot's suggestions live in one comment at the end (`<!-- zen:alternatives … -->`),
  anchored to the text they cover. Notes and the changelog are ordinary `## Notes` and
  `## Changelog` sections.
- **Markdown shows as it reads**, as in Obsidian's live preview: marks are hidden; a line's own marks
  (`#`, `>`, `- `) show on the line being edited, inline ones (`**`, links, math) only on the
  piece the caret touches; `$…$` and `$$…$$` math is drawn by KaTeX, loaded only for notes with math; a click opens a link (web links in a new tab, `[[wiki links]]` and `.md`
  links as notes here; Ctrl/Cmd+click on a link showing its source).
- **Reading is the file viewer's**, with its reading aids; writing brings the tools. Two panels of
  tabs hold the tools: the left one works on the text (alternatives, check, ghosts), the right one
  is about the note (notes, versions with the changelog, properties).
- **Versions.** One open version at a time (the file); closing it takes a changelog line (zenbot
  suggests one) and freezes a copy in `.versions/<note>/vN.md`. Old versions open read-only and can
  be branched from. `scripts/zen.ts` is zenbot's side: it edits only when no version is open, and
  only suggests otherwise.
- **AI, through the zen kernel**, with instructions in zenbot skills (`writing/proofread`, `writing/marks`,
  `writing/shrink`, `writing/find-notes`, `writing/changelog`) so they learn without code changes.

Everywhere: `Alt+W` back to reading (once saved) · `Alt+L` left panel, then its next tab, then closed ·
`Alt+R` the same for the right panel · `Alt+O` open a note (browse the vault's folders, or type to
search every note) · `Ctrl+S` save (it also saves as you type). With the pointer resting on text
with alternatives, the arrow keys flip it in place (typing gives them back to the caret).

The same four keys act on the tab of the panel opened last:

| Tab | `Alt+↑↓` | `Alt+Enter` | `Alt+Del` | `Alt+G` |
|---|---|---|---|---|
| alternatives | flip in place | versions of the selection (↑↓, Enter default, type + Enter adds, Esc) | remove the one showing | 🤖 versions |
| check | previous / next fix or mark | accept (a mark: done) | reject | check the piece (`Alt+P`: fixes or marks, which only point at weak, long and filler text) |
| ghosts | previous / next cut, else ghost | cut it (ghosted) · bring back | keep the words · delete | ghost the selection (`Alt+K`: keep it, ghost the rest; `Alt+P`: 🤖 Shrink, read once, then 10, 20, 30, 50%, off) |
| notes | proposed notes | keep | dismiss | find in wiki (`Alt+S`: send the selection here) |
| versions | choose | open, read-only | | close this version |

A new note is written only once it has something past its H1, and a note changed on disk is
never overwritten.

Shrink's cuts are struck through and faded; a reworded one shows its shorter words after it.
A cut you take is ghosted, or, when reworded, made an alternative with the shorter words showing,
so nothing is lost.

## Run

```bash
npm ci
ZEN_WEB_PORT=4401 npm run dev   # the server with Vite's hot reload (the installed app uses 4400)
npm run check            # types + unit tests
node tests/e2e.mjs       # the writer: the real server and headless Chromium on a scratch folder
node tests/zen-e2e.mjs   # the zen app: the same, with a stand-in kernel (HTTP and WebSocket)
```

zenbot's `scripts/upgrade.sh` builds this folder with the kernel and `scripts/web-install.sh`
runs the build as the user unit `zen-web` on 127.0.0.1:4400, from
`~/.local/share/zen-web/current`, so work in the checkout never touches it.

Settings (environment): `ZEN_WEB_ROOT` (the vault, default `~/.zenbot/global`), `ZEN_WEB_HOME`
(the Files view's home, default `$HOME`), `ZEN_BIN` (default `~/.zenbot/bin/zen`), `ZEN_WEB_PORT`
(4400), `ZEN_WEB_MODEL` (`claude/claude-sonnet-5-5`), `ZEN_URL`, `ZEN_TOKEN_FILE`,
`ZEN_WEB_SKILLS`, `ZEN_WEB_AUTH_HEADER` (the proxy's login header, default exe.dev's
`X-ExeDev-UserID`), `ZEN_WEB_AUTH` (`off` drops the login-header check; local tests only),
`ZEN_WEB_DEV_HOSTS` (host names Vite's dev server accepts besides localhost).
AI goes through the zen kernel (`POST /api/complete`, `POST /api/search`); its token never reaches
the page.

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
