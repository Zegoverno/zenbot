# zenbot — how it works and where things are

zenbot is a personal agent harness. The owner hands it jobs; it runs them on the owner's own
Claude Code and Codex subscriptions, with zenbot's prompt, tools, memory and history. Each module's
header comment is its design note: read it before changing the module.

The main Matrix room uses a channel session shown under Channels on the board. The bridge
replaces archived or missing sessions automatically; authentication and server errors do not
create replacements.

```
 web/ (zen web: browser)   zen (command line, scripts)   zen-matrix (Matrix rooms)
        │ HTTP + WebSocket on 127.0.0.1, owner token
 zend: API · sessions/tape · context compiler · tools · System One · brain · jobs
        │ JSON-RPC over stdio            │ SQLite file        │ HTTPS
 zen-engine: Claude Code CLI,           ~/.zenbot/           OpenRouter (System One,
 codex app-server, faux                 zen.sqlite3          embeddings), web search
```

## Crates

| Crate | What | Runs as |
|---|---|---|
| `crates/zend` | The kernel: API, sessions, the tape, context, tools, System One, memory, jobs | systemd service `zenbot`, `~/.zenbot/bin/zend` |
| `crates/zen-engine` | The worker: runs each turn on the Claude Code or Codex CLI | child process of `zend` |
| `crates/zen` | The command line for scripts: `zen ask`, the shared table's commands (`zen jobs`, `zen memory`, …), `status`, `login`, `brain`, `s1`; no command prints help, piped input runs `zen ask`; `zen --commands` gives the web chat its slash commands | `~/.zenbot/bin/zen`, linked from `~/.local/bin/zen` |
| `crates/zen-client` | The kernel client and the shared command table (`commands.rs`) | library of `zen` and `zen-matrix` |
| `crates/zen-proto` | Typed worker and client protocol messages, shared helpers | library |
| `crates/zen-matrix` | Matrix rooms ↔ sessions (its own Cargo workspace) | systemd service `zen-matrix` |

## The web app

`web/` is zen in the browser: a Svelte page (`src/zen/`: the board with Sessions · Files · Tasks ·
Agents, a session's chat with the side panel, and Files with a safe Markdown preview and plain
textarea edit/save fallback; rich reader/writer features are optional plugins) and a
Node server (`server/`) that serves it on 127.0.0.1:4400 and relays the kernel with the owner token
added on the server, so the token never reaches the page. It also shows the home folder
(`home.ts`), takes chat attachments into `~/inbox` (`uploads.ts`) and runs the shared commands
through the `zen` command line (`commands.ts`). It trusts a signing-in proxy in front: every
request needs the proxy's login header and every write `X-Zen-Web: 1`. `upgrade.sh` builds it
with the binaries and `scripts/web-install.sh` runs it as the user unit `zen-web`
(`~/.local/share/zen-web/current`, settings in `~/.config/zen-web.env`). `web/README.md` has its
features, settings and security rules; `web/AGENTS.md` how to work on it.

## A turn

1. A client sends a prompt on the session's WebSocket. One that arrives during a turn queues
   (`pending_prompts`); a `steer` prompt stops the turn at the next tool boundary and runs as a
   correction (`turns.rs`).
2. The prompt goes on the **tape** (`tape.rs`): each session's append-only chain of numbered,
   hash-linked blocks, mirrored to `~/.zenbot/sessions/<id>.jsonl`. The tape is the source of truth;
   engine sessions are a disposable cache.
3. `compile.rs` sends, in an order that keeps the provider's prompt cache: the envelope
   (instructions and tools, fixed for the session) → a summary of older turns → the history → the
   prompt. Past the soft limit older turns are summarized in the background (`compact.rs`); the
   `history` tool reads any block back.
4. The worker streams text; each tool call comes back as `tool.call`. The kernel runs it
   (`dispatch.rs`: refusals by kind of session, then `agent.rs` or `tools.rs`), masks secrets,
   caps and saves long output, records it in `tool_calls`, and answers. Read-only calls may
   overlap; any other runs alone, in order.
5. At the end the kernel records the turn (`turns`, `model_calls`, `measure.rs`), System One keeps
   owner sentences worth remembering (`memory_capture`), the assist model names the session and
   suggests the next prompt (`assist.rs`), and the indexer adds the turn to search. A watchdog stops
   stalled turns.

## Context

The instructions are the prompt files under `~/.zenbot` (`layout.rs`): the session's agent's
`agents/<name>/SOUL.md` and `IDENTITY.md` (zenbot unless the owner picked another, see Agents),
`AGENTS.md` (the environment), `USER.md` (the owner), short-term memory as of the
session's start, the skills index, then every `AGENTS.md`/`CLAUDE.md` from `/` down to the
workspace (`context.rs`). A project's instruction file found later by a tool is attached to that
tool's result, once per session (tape kind `context`). Missing prompt files get defaults from
`crates/zend/defaults/`; the owner's are never overwritten. Skills, MCP tools, memory records and
past sessions load on demand as tool results, so the prefix never changes mid-session.

## Tools

One list per session, in a fixed order (`agent.rs::specs`):

- Files and shell (`tools.rs`): `bash` (on the host, as the owner, in the workspace; the kernel's
  secret variables removed), `read` (text, images, PDF pages), `write`, `edit`.
- Recall: `history` (the session's messages, or another of the same agent's), `search` (`search.rs`: memories, wiki pages, records and sessions, scoped by agent; exact
  names, FTS5 and sqlite-vec merged, reranked by System One, following typed links between
  records), `remember` (`memory.rs`), `brain` (`brain/`: record files).
- Web (`web.rs`): `web_search` (SearXNG, or Brave/Tavily with a key), `web_fetch`.
- Skills: `find_skills`, `load_skill` (`skills.rs`), `save_skill` (`workshop.rs`: live at once,
  duplicates refused, each save a commit in the skills folder).
- MCP (`mcp.rs`): `find_tools`, `load_tool`, `call_tool` over the servers in `~/.zenbot/mcp.json`
  and the agent's own `agents/<name>/mcp.json`.
- Work: `ask` (questions to the owner), `verify` (`agent.rs`: the kernel runs the checks, a fresh
  verifier session judges the rest), `delegate` (`delegate.rs`: subagents in parallel, each optionally in a `role`
  (`roles.rs`, below; with neither a role nor a model, System One may pick one, `role_pick`); on the task's model, else the role's, else the other agent's, else the roles
  file's `default_model` when served, else the parent's; zenbot may hand a task to another agent), `schedule` (`jobs/`), `judge` (System One over many items; when System
  One is on).

Subagents and job runs get no `ask`, `delegate` or `schedule`; a verifier gets `bash`, `read` and
`submit_verdict`. A session's agent narrows the list further (Agents), and a subagent's role
further still.

Roles (`roles.rs`) are profiles for subagents, not agents: each agent's `agents/<name>/roles.toml`
(the agent keeps it, so a learning loop can tune it; written from `defaults/agent/roles.toml` when
missing: researcher, coder, reviewer) holds `default_model` and `[roles.<name>]` with
`description`, `model`, `effort`, `tools` (narrows, never widens: `Agent::in_role`, enforced at
each of the child's turns by the tool list and `dispatch.rs`) and `skills` (each SKILL.md attached
to the brief, not the instructions). The role is the delegating agent's; the child records it in
`sessions.role`. `delegate`'s description lists the roles as the session's first tool list had them
(the prefix stays fixed); a call checks the current file, and a broken file or unknown role fails it.

## Agents

Every session is one agent's (`agents.rs`, `sessions.agent`): zenbot by default, or one the owner
made (`agents new <name>`, `POST /api/agents`). An agent is a folder `agents/<name>/`: `SOUL.md`
(the owner's), `IDENTITY.md` and `roles.toml` (the agent keeps them), an optional `agent.toml` and `skills/`. Each agent
has a vault (`Agent::vault`): zenbot's is `global/`; another's is private, `global/agents/<name>/`
(`AGENTS.md`, `MEMORY.md`, `wiki/`, `memory/`, `entities/`; written by `agents new`, or at its next
turn), moved to its backup on `agents rm`. Its short-term memory (`memories.agent`), `remember`,
`brain put` and the sleep stay in its vault (even with `global = "write"`); `brain get`/`list` see
its own records, plus the global ones when `global` (agent.toml: none, read or write) allows, and
nothing of another vault shows through (an id taken there is just taken; links to it are left out);
zenbot sees every vault. Sessions are private: `history` reads, and `search` finds, only the agent's
own sessions (zenbot's reach all; the owner's clients show every one). The search index covers every
vault (`search_docs.agent` says whose: a turn its session's agent); an agent's search sees its own
documents, plus the global memories, pages and records when `global` allows (`search::Reach`).
`AGENTS.md` and `USER.md` are shared. zenbot can hand a task to another agent (`delegate` with
`agent`, per task or for all; no other agent can): the child `subagent` session is that agent's,
with its instructions, tools, limits and vault, in its workspace (else the kernel's), on the task's
model, else its role's, else the agent's, else zenbot's `default_model`, else the parent's. An agent may have its own MCP servers
(`agents/<name>/mcp.json`, no OAuth), seen only by it and winning a name clash with a global one;
its stdio servers run in its sandbox (not at all without bubblewrap and pasta), global ones as the owner.
`agent.toml` otherwise only narrows what zenbot has:
`tools`/`deny` (built-in tools), `mcp` (the global servers from `~/.zenbot/mcp.json` it may use:
opt-in, none when missing, since they run as the owner; zenbot's default is all), `shared_skills`, and its sessions'
`model`, `workspace` and `context` (instruction parts). The kernel enforces it: the tool list is
filtered (`agent::specs_for`), `dispatch.rs` refuses any other tool, MCP discovery and calls see
only its servers, skills come from its folder (then the shared one), `save_skill` writes to its
folder, and `write`/`edit` never change an `agent.toml` or an agent's `mcp.json`. Settings are read when a turn starts; a
broken file fails the turn. Subagents (unless zenbot names another agent) and verifiers inherit the parent's agent; a job runs as the
agent that scheduled it (the owner may pick one), never another, and an agent other than zenbot
changes, runs or removes only its own jobs and makes a script job only if it has `bash`. With `bash` an agent can still
Every agent but zenbot is sandboxed (`sandbox.rs`): `Agent::mounts` lists what it reaches, read-write
its workspace (`workspace`, else its own `agents/<name>/work`, never the owner's home), its vault, its
`IDENTITY.md`, `roles.toml` and extra `write` paths; read-only the rest of its folder (it can't change or create
`agent.toml`, `mcp.json`, `SOUL.md`, `skills/`), the system folders, `USER.md`, `AGENTS.md`, `~/.local/{node,bin}`, `~/.cargo/bin`, `~/.gitconfig`, extra `read`
paths and the global vault when `global` allows (writable when write; other agents' vaults hidden).
The owner's home is an empty tmpfs, `/tmp` private. `bash`, `verify` checks and its script jobs run
in bubblewrap built from that list, inside pasta: a network of its own with internet but not the
host's loopback (the kernel, the web app, local engines) nor gateway (either missing: an error, never
unsandboxed); `read`/`write`/`edit` refuse
paths outside it (links resolved). `workspace`, `read` and `write` can't name the zen home or any of
it but the agent's own folder and vault (in its folder, `workspace` and `write` only under `work/`).

## System One

Fast, cheap decisions (`s1/`): each is a TOML file in `crates/zend/s1/` (owner overrides in
`~/.zenbot/s1/`), with its questions, evidence, a policy (costs or rules), an `unavailable`
outcome, a ladder of approved OpenRouter models and test cases. `catalog.rs` fixes what each
caller sends and accepts; `check.rs` validates files (`zen s1 check`); every call is recorded in
`decisions`. Decisions: `memory_capture`, `memory_durable`, `memory_disposition`, `memory_long`,
`search_rerank`, `web_rerank`, `web_focus`, `skill_duplicate`, `role_pick` (a delegated task's
role when it names none), `judge`. Off unless
`ZEN_S1_MODEL` (or `~/.zenbot/s1/models.toml`) and `OPENROUTER_API_KEY` are set.

## Memory and knowledge

- Short-term memory: `~/.zenbot/global/MEMORY.md` (another agent's: `MEMORY.md` in its vault),
  rows of `memories`, in every session of that agent's instructions; the owner may edit the file (read back before the next use).
- The second brain is an Obsidian vault, `~/.zenbot/global`. Knowledge lives in one place, its wiki
  (`wiki.rs`): flat pages in `global/wiki/` named by their title (`Bayes' theorem.md`), YAML
  frontmatter (`type`, `aliases`), inline `[[links]]`; the owner writes them in Obsidian and the
  agent with read/write/edit, following the vault's `AGENTS.md`. The kernel only reads them: search
  indexes each page's whole body by its file name.
- Tasks (`tasks.rs`): the open `- [ ]` lines of each `global/Projects/<project>/tasks.md`, in the
  Obsidian Tasks emoji format, parsed and sorted for `GET /api/tasks` and the `tasks` command. The
  owner ticks and edits them in Obsidian; the kernel never writes them. A task handed to an agent
  (`POST /api/tasks/{id}/agent`) is linked to its session in the `task_agents` table, by its 🆔.
- Records (`brain/records.rs`): entities (people, organizations, projects) and long-term memory,
  one Markdown file each with JSON frontmatter (evidence, typed links); the files are the source of
  truth, `brain_*` tables an index rebuilt from them. `brain put` refuses new knowledge records;
  old ones (kind folders under `wiki/`) are still indexed, read and linted.
- The sleep (`brain/sleep.rs`; nightly, when memory passes its size, or `zen memory sleep`):
  System One judges each entry against the whole short-term memory; lasting ones move to records,
  finished ones (settled, superseded, repeated) leave too, to records when lasting, else to the
  archive. Still over its size, the least active leave until it fits. Nothing is deleted.

## Jobs

One scheduler in the kernel (`jobs/`, tables `jobs` and `job_runs`). System jobs: `sleep`,
`wiki` (index and lint records), `skills` (archive skills unused for 90 days), `prune` (old saved
outputs, backups, engine sessions, eval runs) and `engines` (`scripts/update-engines.sh`). Agent
jobs run a prompt in a fresh `job` session and report its final answer (or a `report_file`);
`[SILENT]` means nothing to say. Script jobs run a command with no model. Jobs are live when
created, by the owner or the agent; a report can be posted into one of the owner's sessions
(`post_to`). `ZEN_JOBS=0` turns the scheduler off.

## Worker protocol

JSON-RPC 2.0, one JSON object per line on the worker's stdio (`crates/zen-proto/src/worker.rs`).
Kernel → worker: `ping`, `models.list`, `turn.start` (session, turn, model, effort, the compiled
messages and tools), `turn.abort`, `complete` (a tool-free completion, for summaries and the assist
model). Worker → kernel: `tool.call` (a request the kernel answers with the result) and the
notifications `turn.delta`, `turn.thinking`, `turn.message`, `turn.usage`, `turn.fallback` (a
hard usage limit moved the turn to the other subscription engine) and `turn.end`. Models are
`<engine>/<model>`; `faux/smoke` (`ZEN_FAUX=1`) runs scripted turns for tests. Engines run with
their own tools off (Claude Code gets zenbot's tools through `zen-engine mcp-bridge`; Codex gets
them as dynamic tools), so every action goes through the kernel.

## Client protocol

HTTP and a WebSocket on `127.0.0.1:8100` (`ZEN_BIND`, `ZEN_PORT`), the owner token in the
`Authorization: Bearer` header only (`~/.zenbot/token`). `GET /health` (ok, database, workers,
busy turns, commit) needs no token; the rest are under `/api` (`main.rs`):

| Route | What |
|---|---|
| `GET/POST /sessions`, `GET/PATCH /sessions/{id}`, `GET /board` | sessions (`agent` picks who a new one is), their messages, the board; `PATCH` with `archived: true` takes the owner's `outcome` (`done`, `partial`, `not-done`) and `note`, and returns `continued_by` (the new session) after `partial`; `GET /sessions/{id}` includes the open `suggestion` |
| `GET/POST /agents` | the agents and what each may use; make one (`name`, `description`) |
| `GET/PUT/DELETE /agents/{name}` | an agent's files (`soul`, `identity`, `settings` as text) and session count; replace any of them (settings checked first, old files backed up); remove one (not zenbot; its folder moves to `backups/agents/`) |
| `GET /sessions/{id}/ws` | the session's events; send `{"type":"prompt","text":…,"mode":"steer"?}` or `{"type":"abort"}` |
| `GET /models`, `GET /version`, `GET/POST /upgrade` | models, update check, self-upgrade |
| `POST /complete` | one completion without tools (`model`, `system`, `prompt`, `purpose`), for the owner's apps on this machine (including optional writing apps); logged, not on a tape |
| `POST /search` | the search tool for the owner's apps (`query`, `scope` default `knowledge`, `limit`): words and meaning, reranked by System One, as JSON results (including optional note-finding apps) |
| `GET /memory`, `POST /memory/sleep`, `POST /brain` | memory and records |
| `GET /skills`, `GET /suggestions` | skill use, next-prompt suggestions |
| `GET /tasks` | the owner's open vault tasks, grouped by project, most pressing first (`tasks.rs`, read-only), each with its agent's session and state |
| `POST /tasks/{id}/agent` | hand open task `id` (its 🆔) to an agent: a new session briefed with it, linked in `task_agents`; an existing agent comes back |
| `GET/POST /jobs`, `GET /jobs/runs`, `PATCH/DELETE /jobs/{name}`, `POST /jobs/{name}/run` | jobs |
| `GET /mcp`, `POST /mcp/login`, `POST /mcp/login/{flow}`, `GET /mcp/pending`, `POST /mcp/pending/{id}` | MCP servers, sign-in, forms |
| `GET /s1`, `/s1/check`, `/s1/models`, `POST /s1/replay`, `POST /s1/compare` | System One |

WebSocket events (`crates/zen-proto/src/client.rs`): `message`, `busy`, `delta`, `thinking`,
`tool_start`, `tool_end`, `end`, `child_end`, `idle`, `title`, `suggestion`, `model`, `status`,
`questions`, `error`, `resync`.

## Commands

`crates/zen-client/src/commands.rs` is the one command table: `/x` in the web chat, `!x` in
Matrix and `zen x` on the command line are the same command. Shared: `new` (`new @garry` starts
a session with another agent), `model`, `effort`, `rename`, `archive [done|partial|not-done [note]]` (how the session ended: the learning loop's ground truth; `partial` starts a linked "Continue:" session for the same agent whose suggested first prompt is a handoff brief from the summary model, `handoff.rs`), `steer`, `agents`, `tasks`, `jobs`, `skills`, `memory`, `mcp`, `models`, `suggestions`,
`status`, `upgrade`. The web chat's own (scope `Screen`): `resume`, `board`, `files`, `open`, `close`, `mouse`,
`restart`, `help`, `exit`; it gets the list from `zen --commands`. The command line's own: `ask`, `sessions`, `brain`, `s1`,
`login`, `mcp-pending`, `mcp-respond`. Tests keep the command line and the table in step.

## Files under `~/.zenbot`

| Path | What |
|---|---|
| `env`, `token` | the service's settings and the API token (private) |
| `zen.sqlite3` | the database (WAL; private) |
| `sessions/` | the tape mirrored as JSONL, one file per session |
| `agents/<name>/SOUL.md`, `IDENTITY.md`, `AGENTS.md`, `USER.md` | prompt files (backups in `backups/prompt-files/`) |
| `agents/<name>/agent.toml`, `agents/<name>/skills/` | what an agent may use (the owner's), its own skills |
| `agents/<name>/roles.toml` | its roles for subagents (the agent's; `roles.rs`) |
| `global/MEMORY.md`, `global/memory/`, `global/entities/`, `global/wiki/`, `global/skills/` | memory, records, wiki pages, skills |
| `global/Projects/<project>/tasks.md` | the owner's tasks (Obsidian Tasks format); the kernel only reads them |
| `mcp.json`, `mcp-auth/` | MCP servers and their sign-ins |
| `s1/` | the owner's decision files and `models.toml` |
| `outputs/` | full tool outputs too long to show |
| `bin/`, `version`, `web.version`, `upgrades/`, `upgrade.log`, `backups/` | the installed binaries, web app and upgrades |
| `engine/` | the engines' own working folders |
| `matrix/`, `matrix.env` | the Matrix bridge (private) |

## Database

`crates/zend/sqlite-migrations/` (run at start by `storage.rs`; sqlite-vec and FTS5 built in).
Tables: `sessions`, `tape_events`, `tape_exports`, `pending_prompts`, `turns`, `model_calls`,
`tool_calls`, `envelopes`, `compactions`, `session_decisions` (old `/done` verdicts, history only; `sessions.outcome` replaces them),
`prompt_suggestions`, `memories`, `sleep_runs`, `brain_records`, `brain_aliases`, `brain_links`,
`brain_skill_uses`, `search_docs` (+ `search_docs_fts`), `search_state`, `search_queue`,
`searches`, `jobs`, `job_runs`, `decisions`, `s1_states`, `s1_outcomes`, `mcp_interactions`,
`task_agents`.

## Settings

In `~/.zenbot/env`. Read once in `config.rs` (default in brackets):

- Models: `ZEN_DEFAULT_MODEL` [claude/claude-opus-5-5], `ZEN_SUMMARY_MODEL`
  [claude/claude-sonnet-5-5], `ZEN_ASSIST_MODEL` [claude/claude-haiku-5-5], `ZEN_CACHE_TTL`
  [claude=3600].
- Context: `ZEN_CONTEXT_TOKENS` [unset: each model's window, 200000 when unknown], `ZEN_COMPACT_SOFT` [0.7], `ZEN_COMPACT_HARD` [0.9],
  `ZEN_COMPACT_KEEP` [0.3], `ZEN_COMPACT_IDLE_SECS` [300], `ZEN_SOUL_CHARS` [4000],
  `ZEN_IDENTITY_CHARS` [4000], `ZEN_AGENTS_CHARS` [12000], `ZEN_USER_CHARS` [3000],
  `ZEN_SKILL_INDEX_CHARS` [2500], `ZEN_CLAUDE_RESUME`, `ZEN_CODEX_RESUME` [on].
- System One and embeddings: `ZEN_S1_MODEL`, `ZEN_S1_URL`, `ZEN_S1_PRIVATE` [on],
  `ZEN_S1_CONCURRENCY` [8], `ZEN_JUDGE_TOOL` [on], `ZEN_EMBED` [on], `ZEN_EMBED_MODEL`,
  `ZEN_EMBED_URL`, `OPENROUTER_API_KEY`.
- Memory and search: `ZEN_MEMORY_CHARS` [4000], `ZEN_INDEX_SECS` [20], `ZEN_SEARCH_PROVIDER`,
  `ZEN_SEARXNG_URL` [http://127.0.0.1:8888], `ZEN_SEARCH_RERANK` [on], `BRAVE_API_KEY`,
  `TAVILY_API_KEY`.
- Jobs and turns: `ZEN_TZ` [UTC], `ZEN_JOBS` [on], `ZEN_JOBS_TICK` [60],
  `ZEN_ENGINE_CONCURRENCY` [3], `ZEN_TURN_IDLE_SECS` [600], `ZEN_TURN_ABORT_GRACE_SECS` [30],
  `ZEN_SUGGEST` [on], `ZEN_UPDATE_CHECK_SECS` [3600], `ZEN_QUOTA_FAILOVER`.
- Boot and paths: `ZEN_TOKEN`, `ZEN_PORT` [8100], `ZEN_BIND` [127.0.0.1], `ZEN_WORKSPACE` [~],
  `ZEN_REPO` [~/zenbot], `ZEN_HOME` [~/.zenbot], `DATABASE_URL`, `ZEN_SKILLS_DIR`, `ZEN_WIKI_DIR`,
  `ZEN_MCP_CONFIG`, `ZEN_WORKERS` [engine], `ZEN_ENGINE_CMD`, `ZEN_MODELS`, `ZEN_FAUX`.

## Scripts

| Script | What |
|---|---|
| `install.sh` | install on a fresh VM (INSTALL.md) |
| `scripts/check.sh` | what CI runs: the marketplace checker's self-test, the web app's checks and e2e tests, build, unit tests, clippy, e2e |
| `scripts/e2e.sh [filter]` | end-to-end scenarios on throwaway kernels with the faux model |
| `scripts/upgrade.sh [--check]` | build this checkout and install it once no session is working |
| `scripts/apply-upgrade.sh` | the install step: back up the database, swap, health-check, roll back; then the web app |
| `scripts/web-install.sh` | build `web/`, and run a build as the user unit `zen-web` on :4400, with rollback |
| `scripts/self-update.sh` | `zen upgrade`: pull `main`, then `upgrade.sh` |
| `scripts/dev.sh` | a dev kernel on :18100 with its own home and database |
| `scripts/db.sh`, `scripts/storage.py` | SQLite helpers and online backups |
| `scripts/update-engines.sh [--check]` | keep the Claude Code and Codex CLIs current, with rollback |
| `scripts/searxng.sh` | install and check the native SearXNG web search |
| `scripts/matrix.sh`, `scripts/matrix-e2e.sh` | build and install the Matrix bridge; its end-to-end test |
| `scripts/marketplace-check.mjs [--base <ref>] [--self-test]` | check `marketplace/plugins.json` (rules, and the archive of each new or changed entry); CI runs it on pull requests touching `marketplace/` |
| `scripts/marketplace-page.mjs` | build the static marketplace page `marketplace/site/` (CI deploys it to GitHub Pages from `main`) |
| `scripts/eval.sh` (+ `eval-*`) | compare harness versions on `evals/tasks/` |

## Plugin host

`crates/zend/src/plugins.rs` validates manifests, approves permissions, installs SHA256-pinned
release archives and serves `/api/plugins`. `sandbox.rs` pins grants with file descriptors before
bubblewrap binds them; servers join MCP discovery through `mcp.rs`. Installed state and opener
defaults live in migration `0007_plugins.sql`. UI slots, Games and Settings live in
`web/src/zen/`; `PluginFrame.svelte` hosts opaque-origin MCP Apps. Plugin messages create drafts,
never autonomous chat turns. A plugin granted `ai` may sample (`sample`: the kernel's tool-free completion, assist model unless
its hints name a known one, no token cap) while its AI switch (`plugins.ai_enabled`, migration
`0008_plugin_ai.sql`, `PATCH { ai }`) is on; each request goes in `plugin_calls`.
See [PLUGINS.md](PLUGINS.md) for authoring and the curated marketplace workflow.
