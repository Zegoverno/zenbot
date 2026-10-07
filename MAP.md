# zenbot — Codebase Map

> **Purpose:** a blast-radius guide for coding agents. Before touching anything, find it here to see
> what it does, what it depends on and what depends on it. The map is not the territory: read the
> code before changing it, and if the two disagree, the code wins (then fix the map).
>
> Written from the code on 2026-10-06; updated after PRs #8, #9, #10 and #13, for Phase 1
> (prompt files, memory, skills, the workflow removed) and for Phase 2 (web search and fetch, the MCP
> client) on 2026-10-07. Where something couldn't be confirmed in the code it says "unverified". Line counts are approximate and only show which files are big.

---

## Quick orientation

```
 owner
   │  terminal                        browser
   ▼                                    ▼
 zen (CLI, crates/zen)            web UI (crates/zend/web/index.html, frozen)
   │  HTTP /api/* + WebSocket /api/sessions/{id}/ws   (Bearer token from ~/.zenbot/token)
   ▼
 zend (kernel, crates/zend) ───── sqlx ─────▶ Postgres + pgvector (Docker, deploy/compose.yaml)
   │  owns all state, runs every tool call, keeps the tape
   │  web.rs ──HTTP──▶ SearXNG (Docker, 127.0.0.1:8888) or Brave / Tavily; public web pages
   │  mcp.rs ──stdio / streamable HTTP──▶ the owner's MCP servers (~/.zenbot/mcp.json)
   │  JSON-RPC 2.0 over stdio, one JSON object per line (docs/worker-protocol.md)
   ├──▶ zen-engine (crates/zen-engine)            default worker `engine`
   │      ├─ claude: `claude -p --tools ""` + MCP config pointing at
   │      │          `zen-engine mcp-bridge <socket>` ──Unix socket──▶ engine ──tool.call──▶ zend
   │      ├─ codex:  `codex app-server`, shell/apps/plugins off, zenbot tools as dynamic tools
   │      └─ faux:   scripted model `faux/smoke` (ZEN_FAUX=1), for tests
   └──▶ zen-mind (packages/mind, Node 22)        optional worker `pi`
          Pi agent loop: direct ChatGPT sign-in (~/.zenbot/auth.json), OpenRouter classifiers,
          faux provider (ZEN_FAUX=1)
```

- **The kernel owns all state and executes every tool call.** Workers are stateless: each
  `turn.start` carries the whole context (system prompt, history, prompt, tools), and every tool the
  model calls comes back to the kernel as `tool.call`. Engines run with their own tools off (Claude
  Code `--tools ""` plus `--strict-mcp-config`; Codex `shell_tool: false`, `unified_exec: false`,
  apps and plugins off, web search disabled). Keep it that way.
- **The tape is the truth.** Each session is an append-only, hash-chained list of blocks in
  `tape_events`. Engine-side sessions (Claude Code's) are only a cache of it.
- **One process tree.** systemd runs `zend` (`deploy/zenbot.service`); `zend` starts the workers
  as children (`bash -lc <cmd>`) and restarts them when they exit (`workers.rs::supervise`).
- **Never restart `zend` yourself** (it kills the session you run in). Use `scripts/upgrade.sh`.

---

## Layer 1 — Kernel `zend` (`crates/zend`)

Single binary (axum, sqlx, tokio). Modules are declared in `main.rs`; most share `App` through
`use super::*`. ~7,100 lines of Rust plus migrations, default prompt files and skills, the verifier's
prompt and the web UI.

### Files

| File | Lines | Role | Main items | Depends on | Used by |
|---|---|---|---|---|---|
| `src/main.rs` | 240 | Startup and shared state. Reads env, writes missing default prompt files and skills (`defaults::install`), connects to Postgres, runs migrations (`set_ignore_missing(true)` so a rolled-back build starts on a newer schema), repairs the tape, spawns workers, starts background tasks, builds the router | `App` (incl. `hubs`), `zen_home` (`ZEN_HOME`, else `~/.zenbot`), `subscribe`/`unsubscribe`/`emit` (a session's event hub exists only while a client is connected; `emit` with no client sends nothing), `env_num`, `load_messages`, `context_in`, `append_tape` | every module | everything (via `App`) |
| `src/api.rs` | 346 | HTTP handlers and the WebSocket (docs/client-protocol.md). Token auth middleware (header `Authorization: Bearer` or `?token=`, compared in constant time). A WebSocket `prompt` starts its turn in a separate task, so an `abort` is read while the turn is prepared. `run_sleep` runs the sleep in its own task, so a client that stops waiting doesn't cut it short | `auth`, `same_secret`, `health`, `version`, `upgrade_*`, `list_models`, `*_session`, `decide`, `DECISIONS`, `list_memory`, `run_sleep`, `mcp_status`, `handle_socket` | `turns` (start/abort), `memory`, `mcp`, `score`, `workers`, `update` | router in `main.rs` |
| `src/turns.rs` | 553 | A turn's lifecycle: start (owner or kernel), compile what it sends (tools and instructions from `agent::specs` / `agent::system_for` by the session's kind), record it in `turns`, finish, abort, watchdog, child sessions (verifiers). `Turn.kind` is read once at start. A summary made inline at the hard limit is `hold`-counted as a running tool so the watchdog waits, and an abort stops it. `record_turn` returns the row plus the session's parent and cost in one query | `Turn`, `Origin`, `Recorded`, `start_turn`, `start_kernel_turn`, `begin_turn`, `still_ours`, `hold`, `finish_turn`, `abort_turn`, `run_child`, `watchdog`, `record_turn`, `session_cost` | `agent`, `compile`, `compact`, `measure`, `tape`, `workers` | `api`, `dispatch`, `agent`, `workers` |
| `src/dispatch.rs` | 202 | Handles every message from workers: `tool.call` (refused after a tool ended the turn, `agent::refuse`; a verifier may run only `bash`, `read`, `submit_verdict`; then `agent::run_tool` or `tools::execute`), `turn.delta`/`turn.thinking`, `turn.message` (tape + `model_calls`), `turn.usage`, `turn.end`. Drops messages not from the turn running on that worker (matched by echoed `turn_id`; by session and worker when a message has none). Attaches newly met AGENTS.md files to tool results | `dispatch`, `handle_incoming`, `touch`, `turn_id` | `agent`, `tools`, `compact`, `context`, `turns` | `main.rs` (spawned task) |
| `src/agent.rs` | 440 | The agent's tools beyond files and the shell, in a fixed order after them (see "Tools the model gets"); runs `ask` (questions block, ends the turn), `verify` (kernel runs criteria commands, a child verifier session judges the rest; `verification` block), `decide` (when a System One model is set; logged as `point = 'tool'`) and the verifier's `submit_verdict`; hands `history`, `remember`, `web_*`, `find_skills`/`load_skill` and `find_tools`/`load_tool`/`call_tool` to their modules. Replaced `flow.rs`: no workflow (docs/brief.md) | `specs`, `system_for`, `read_only`, `run_tool`, `refuse`, `log_decision`, `combine`, `render_results`, `VERIFIER` | `tools` (`run_shell` for checks), `memory`, `skills`, `web`, `mcp`, `compact`, `git`, `secrets`, `score`, `turns` (`run_child`); `steps/verify.md` | `dispatch`, `turns` |
| `src/memory.rs` | 608 | Short-term memory and the sleep (DESIGN.md "Memory and skills"): the `remember` tool (what a tainted session saves counts as `inferred`), rendering into the instructions, export to `~/.zenbot/MEMORY.md`, the hard ceiling (refuse + start a sleep), `sleep` (rank, keep, archive, propose/promote; one at a time), the morning note | `cap`, `render`, `export`, `spec`, `run_tool`, `sleep`, `plan`, `priority`, `questions`, `morning_note`, `last_run`, `list` | `score` (`decide`, `private_ok`), `secrets`, `web::tainted` | `agent`, `compile`, `api` |
| `src/skills.rs` | 409 | Skills in `~/.zenbot/skills/<domain>/<name>/` (`ZEN_SKILLS_DIR`): frontmatter, validation (agentskills.io rules), scan, the index for the instructions, `find_skills` word matching, `load_skill` (SKILL.md or a file inside the skill, cut at 48 KB) | `root`, `frontmatter`, `validate`, `scan`, `index_text`, `find`, `lookup`, `load`, `find_spec`, `load_spec`, `run_tool` | — | `agent`, `compile` |
| `src/defaults.rs` | 74 | Writes the default prompt files and skills (`defaults/`, compiled in) that are missing under `~/.zenbot`; never overwrites; a skill only when its whole folder is missing | `install` | — | `main.rs` |
| `src/web.rs` | 711 | `web_search` and `web_fetch`. Fetch: http(s) to public addresses only (own DNS resolver drops non-public addresses; IP literals and each of at most 5 redirects checked; no proxy), 10 s connect / 30 s / 5 MB, HTML to markdown (`dom_smoothie`, `htmd` fallback), 20,000 characters per call paged by `offset`, 15-minute cache, PDFs saved to `~/.zenbot/outputs/web-*.pdf` for `pdftotext`, `focus` keeps the parts System One judges relevant (`web_focus` decision). Search: Brave or Tavily when a key is set (or `ZEN_SEARCH_PROVIDER`), else SearXNG, which also rescues one failed keyed call; System One reranks (`web_rerank` decision). Every result is wrapped in an `<untrusted>` envelope and taints the session (`sessions.tainted_at`, a `taint` tape block) | `is_public`, `check_url`, `untrusted`, `tainted`, `readable`, `chunks`, `render_hits`, `search_spec`, `fetch_spec`, `run_tool` | `score` (`decide`, `private_ok`), `agent::log_decision`, `secrets`, `tape` | `agent`, `memory`, `mcp` (`untrusted`) |
| `src/mcp.rs` | 628 | MCP client, written against the spec (2025-06-18; no `rmcp`): servers from `~/.zenbot/mcp.json` (`ZEN_MCP_CONFIG`; `mcpServers`, `command`/`args`/`env` for stdio or `url`/`headers` for streamable HTTP, `${VAR}` from the kernel's environment; per server `enabled`, `timeout_s`, `include`/`exclude`, `untrusted`, default true for remote). Config re-read when its mtime changes; servers connected lazily. The model's tool list stays fixed: `find_tools` (word ranking, names as `<server>_<tool>`), `load_tool` (schema as a tool result), `call_tool` (required arguments checked, output masked, over 50 KB cut with the full text in `~/.zenbot/outputs/mcp-*.txt`; untrusted servers' output wrapped and the session tainted) | `config_path`, `expand`, `parse_config`, `sse_messages`, `rank`, `missing_args`, `result_text`, `find_spec`, `load_spec`, `call_spec`, `run_tool`, `status` | `web::untrusted`, `secrets`, `tape` | `agent`, `api` (`/api/mcp`) |
| `src/tools.rs` | 704 | The file and shell tools and their execution: `bash` on `run_shell` (own process group killed on timeout or abort, read-only `bwrap` sandbox for a verifier; also used by `agent::run_check`), `read`, `write`, `edit` (whitespace/line-ending normalized matching). Per-file locks. Output over 50 KB cut to head and tail (off the runtime threads), full text saved under `~/.zenbot/outputs/`. Every result goes through `secrets::mask` | `specs`, `execute`, `run_shell`, `Shell`, `resolve`, `ToolOutput` | `secrets` | `dispatch`, `agent`, `context`, `web`/`mcp` (`ToolOutput`) |
| `src/compile.rs` | 305 | What a turn sends, in cache order: envelope (system prompt + tools, stored once in `envelopes`) → summary → history → prompt with turn context. The system prompt: SOUL.md, AGENTS.md (placeholders filled), USER.md (each capped, cut in the middle), memory and the sleep note, the skills index, project instruction files | `system_prompt`, `cut_middle`, `base_prompt`, `envelope`, `turn_context`, `history`, `Envelope`, `SummaryRef` | `context`, `memory`, `skills`, `tape` | `turns`, `measure` |
| `src/context.rs` | 144 | Instruction files (AGENTS.md, else CLAUDE.md, capped at 32 KB): always one per directory from `/` to the workspace (`~/.zenbot/AGENTS.md` is a prompt file, compile.rs); on demand, a project's file the first time a tool touches a path in it (tape kind `context`) | `always`, `governing`, `paths_in_call`, `attachment`, `read_capped` | `tools::resolve` | `compile`, `dispatch` |
| `src/compact.rs` | 460 | Summaries of older turns: prepared in the background past the soft limit, applied after a pause or at once past the hard limit (`compaction` block). Also the `history` tool, which reads old blocks back by number or search | `Settings`, `summary_model`, `plan`, `prepare`, `pending`, `apply`, `tool_spec`, `history_tool` | `tape`, `workers::complete` | `turns`, `dispatch`, `tools` |
| `src/measure.rs` | 177 | Per-turn record of what was sent and why the prompt cache could or couldn't be reused (`cache_break`: `first`, `instructions`, `summary`, `model`, `engine_session`, `expired`; unexpected: `history`, `miss`) | `previous`, `record`, `break_at_start`, `break_at_end`, `cache_ttl_secs` | `compile` types | `turns` |
| `src/tape.rs` | 70 | The tape: append (advisory lock per session, `seq` + parent + hash computed in SQL by `zen_block_hash`), load, repair (`zen_rechain`) | `Block`, `append`, `load`, `load_all`, `repair` | DB functions from migration 0007 | `compile`, `compact`, `agent`, `turns`, `web`, `mcp`, `main.rs` |
| `src/workers.rs` | 189 | Worker configs from `ZEN_WORKERS`, supervision with backoff (ends orphaned turns on a crash), model and classifier routing, curated model list, effort checks, `complete` for summaries | `Worker`, `worker_configs`, `supervise`, `complete`, `DEFAULT_MODELS`, `collect_models`, `worker_for`, `model_info`, `check_effort` | `mind`, `score` | `main.rs`, `api`, `turns`, `agent`, `compact` |
| `src/mind.rs` | 124 | JSON-RPC client for one worker process (`bash -lc <cmd>`); 30 s default request timeout, `request_within` for longer | `Mind`, `Incoming`, `spawn`, `request`, `request_within`, `respond` | — | `workers`, `main.rs` |
| `src/score.rs` | 283 | Live scoring: a System One classifier answers fixed, versioned questions (`QUESTIONS_VERSION = "v1"`) about a session after a decision or after it goes idle; stored in `session_scores`. Off unless `ZEN_S1_MODEL` is set. Also `decide` (any typed question to System One) and `private_ok` (`ZEN_S1_PRIVATE`) | `scorer`, `decide`, `private_ok`, `questions`, `state`, `score_session`, `idle_loop` | `workers` (`s1.decide`) | `api`, `agent`, `memory`, `web`, `workers`, `main.rs` |
| `src/secrets.rs` | 193 | Masks secrets in tool output: values of the kernel's own secret-looking env vars, `~/.zenbot/token`, `~/.zenbot/auth.json` values, and well-known token prefixes / private key blocks | `mask`, `mask_off_thread` (large text on a blocking thread) | — | `tools`, `agent` (check output, diff), `memory` (memories), `web` (pages), `mcp` (tool output) |
| `src/update.rs` | 176 | Self-update: compares `~/.zenbot/version` with `origin/main` (hourly by default), asks `scripts/fetch-release.sh --check` whether binaries exist, starts `scripts/self-update.sh` on request | `Updater` (`running`, `info`, `check`, `check_periodically`, `start`, `status`) | `git`, scripts | `api`, `main.rs` |
| `src/git.rs` | 64 | Async git with a 60 s limit and an output cap (git is stopped once the cap is reached) | `output`, `git` | — | `update`, `agent` (the verifier's diff) |
| `steps/verify.md` | 9 | The verifier's whole system prompt (`include_str!` in `agent.rs`) | — | — | `agent::system_for` |
| `defaults/` | — | Default `SOUL.md`, `AGENTS.md` (with `{{workspace}}`, `{{home}}`, `{{zen_home}}`, `{{repo}}`), `USER.md`, and the skills `work/brief` (with `references/template.md`) and `work/verify` (`include_str!` in `defaults.rs`) | — | — | `defaults::install` |
| `web/index.html` | 493 | Browser UI, served at `/` (`include_str!`). **Frozen** (AGENTS.md). Uses `/api/models` and `/api/sessions…` | — | API | owner |
| `migrations/*.sql` | — | Schema (see Database) | — | — | `sqlx::migrate!` in `main.rs` |

### Invariants stated in header comments

- `main.rs`: "Owns all state and all side effects."
- `tools.rs`: "The kernel is the only place side effects happen." Tool specs are in a fixed order
  because they are part of the cached prefix.
- `compile.rs` / `docs/context.md`: instructions and tools are fixed for the session; history is
  append-only; anything that changes per turn goes at the end (turn context). Breaking this breaks
  the prompt cache (`measure.rs` will report `history` or `miss`).
- `agent.rs`: **one tool list for every turn of a session** (a verifier has its own), in a fixed
  order, so the cache holds; what's loaded later (skills) arrives as tool results. The kernel, not
  the model, runs the criteria's commands; a verifier can't write and never sees the maker's
  reasoning; nothing runs after a tool that ends the turn (`ask`, `submit_verdict`).
- `tape.rs`: appends to one session are serialized by an advisory lock; the hash is computed by the
  database so every writer hashes the same canonical JSON.
- `dispatch.rs`: the worker gets its answer before the tool call is recorded, so a DB failure can't
  leave the model waiting. Messages from a turn other than the one running on that worker (by
  `turn_id`) are dropped, so a turn the kernel already ended can't leak into the next one.
- `memory.rs`: memory is rendered into the instructions at a session's start and frozen; nothing is
  deleted (dropped entries are archived); one sleep at a time.
- `defaults.rs`: a default file is written only when missing, never over the owner's or the agent's.
- `web.rs`: only public http(s) addresses are fetched, checked at resolve time and on every
  redirect; web content always arrives inside one `<untrusted>` envelope (look-alike markers
  defused) and taints the session, so what that session saves to memory counts as inference.
- `mcp.rs`: MCP tools never join the model's tool list (that would break the prompt cache); they
  are reached through `find_tools` / `load_tool` / `call_tool`, and every call goes through the
  kernel like any other tool call.
- `secrets.rs`: masking applies to what tools return; the command text the model typed is not masked.

---

## Kernel routes

From `main.rs` (router) and `api.rs`. Everything under `/api` needs the token.

| Method | Path | Handler | Does |
|---|---|---|---|
| GET | `/` | `index` | Web UI (no auth; the page asks for the token) |
| GET | `/health` | `health` | No auth. `{ok, db, mind, workers:{name:bool}, busy, version, commit}`; pings every worker. `busy` = running turns + kernel work outside them. Used by `wait_healthy` and `apply-upgrade.sh` (`"busy":0`) |
| GET | `/api/models` | `list_models` | Asks every worker for `models.list`, refreshes routes, returns the curated list (`ZEN_MODELS` order, plus any `faux/*`), `authenticated`, `default`, `scorer` |
| GET | `/api/sessions?archived=` | `list_sessions` | Top-level sessions only (`kind IS NULL`, so verifiers are hidden), with cost |
| POST | `/api/sessions` | `create_session` | `{title?, model?, effort?}` (`state` stays null) |
| GET | `/api/sessions/{id}` | `get_session` | Session, its `message` blocks (with `seq`) and `busy` |
| PATCH | `/api/sessions/{id}` | `update_session` | Title, model, effort (`"default"` clears it), archived |
| GET | `/api/sessions/{id}/ws` | `session_ws` | WebSocket; its session's event hub is created on connect and freed when the last client leaves. Client sends `{type:"prompt",text}` or `{type:"abort"}`. Server events: `message`, `delta`, `thinking`, `tool_start`, `tool_end`, `busy`, `end`, `idle`, `status`, `questions`, `child_end`, `error`, `resync` |
| POST | `/api/sessions/{id}/decision` | `decide` | `{decision: accept\|more\|reshape\|drop, note?}` → `session_decisions`; triggers scoring |
| GET | `/api/memory?tier=` | `list_memory` | Memories of a tier (default `short`), the last sleep, the size |
| POST | `/api/memory/sleep?trigger=` | `run_sleep` | Tidy short-term memory now (`nightly` from `scripts/sleep.sh`, else `owner`) |
| GET | `/api/mcp` | `mcp_status` | `{config, servers:{name:tool count}, problems}` from `~/.zenbot/mcp.json`; connects servers not yet connected |
| GET | `/api/version?refresh=` | `version` | Running commit vs `origin/main` |
| GET/POST | `/api/upgrade` | `upgrade_status` / `upgrade_start` | Start `scripts/self-update.sh`; status = job + last line of `~/.zenbot/upgrade.log` |

---

## Tools the model gets

`tools.rs::specs()` (`bash`, `read`, `write`, `edit`) then `agent.rs::specs()` adds the rest, in a
fixed order, the same every turn: `bash`, `read`, `write`, `edit`, `history`, `ask`, `remember`,
`web_search`, `web_fetch`, `find_skills`, `load_skill`, `find_tools`, `load_tool`, `call_tool`,
`verify`, then `decide` when System One is configured. A verifier session (`kind = 'verifier'`)
gets `bash`, `read`, `submit_verdict` only (`dispatch.rs` refuses anything else). There are no
session states and no state-dependent tools.

| Tool | Executed by | Offered to |
|---|---|---|
| `bash`, `read`, `write`, `edit` | `tools.rs` (bash in a read-only bwrap sandbox for a verifier) | every session (a verifier: `bash`, `read`) |
| `history` | `compact.rs::history_tool` | the owner's sessions |
| `ask` | `agent.rs` (`questions` block and event; ends the turn) | the owner's sessions |
| `remember` | `memory.rs` | the owner's sessions |
| `web_search`, `web_fetch` | `web.rs` (results wrapped as untrusted; the session is tainted) | the owner's sessions |
| `find_skills`, `load_skill` | `skills.rs` | the owner's sessions |
| `find_tools`, `load_tool`, `call_tool` | `mcp.rs` (the owner's MCP servers) | the owner's sessions |
| `verify` | `agent.rs` (criteria commands on `run_shell`; a child verifier via `turns::run_child`; `verification` block) | the owner's sessions |
| `decide` | `agent.rs` → worker `s1.decide`, logged in `decisions` (`point = 'tool'`) | when `ZEN_S1_MODEL` is set and `ZEN_DECIDE_TOOL` isn't `0` |
| `submit_verdict` | `agent.rs` (`verdict` block; ends the turn) | verifiers only |

`sessions.state` is no longer written (null for new sessions; old sessions keep theirs; see Notes).

---

## Layer 2 — Workers

Protocol: `docs/worker-protocol.md`. Kernel → worker: `ping`, `models.list`, `turn.start`,
`turn.abort`, `complete`, `s1.decide` (Pi only). Worker → kernel: `tool.call` (request) and the
notifications `turn.delta`, `turn.thinking`, `turn.message`, `turn.usage`, `turn.end`.
`turn.start` carries the kernel's `turn_id`; workers echo it on every message of the turn and key
running turns by it (not by session), since the session's next turn can start while an ended one is
still stopping. `turn.abort` takes an optional `turn_id` (without it: every turn of the session).
`turn.usage` may carry `fallback_reason` (resume or native history refused).
**A protocol change must update the doc and every worker** (AGENTS.md).

### `zen-engine` (`crates/zen-engine`, ~1,610 lines)

| File | Lines | Role | Depends on | Used by |
|---|---|---|---|---|
| `src/main.rs` | 136 | Entry: `zen-engine` serves the protocol on stdio; `zen-engine mcp-bridge <socket>` runs the bridge. Routes `turn.start`/`complete` by the model's prefix (`claude/`, `codex/`, `faux/`); running turns keyed by `turn_id` | all modules | `zend` (`ZEN_ENGINE_CMD`), Claude CLI (bridge) |
| `src/rpc.rs` | 74 | JSON-RPC over stdio with the kernel | — | everything |
| `src/turn.rs` | 333 | Shared per-turn pieces: `TurnInput`, `TurnCtx` (session and turn ids, `notify` adds both; a Unix socket per turn that forwards tool calls to the kernel as `tool.call`), `StderrTail` (last few KB of a CLI's stderr, quoted in errors), history transcript, seed blocks, `enabled(var)` (on unless `"0"`) | `rpc`, `zen-proto` | `claude`, `codex`, `faux` |
| `src/bridge.rs` | 70 | The stdio MCP server Claude Code launches; answers `tools/list` and `tools/call` by asking the engine over the socket | `turn` socket | Claude CLI |
| `src/claude.rs` | 441 | Runs a turn with `claude -p` (stream-json), `--tools ""`, `--strict-mcp-config`, `--setting-sources ""`, `--system-prompt` replaced with zenbot's, allowed tools `mcp__zen__*`. Engine sessions live in `~/.zenbot/engine/claude` (resume / seed / transcript); unused ones deleted after 30 days. `complete` runs in `~/.zenbot/engine/complete`. Fails the turn on stream lines it doesn't understand, and reports `fallback_reason` | `turn`, `bridge` | `main.rs` |
| `src/codex.rs` | 436 | Runs a turn through `codex app-server` (cwd: an empty placeholder dir under the temp dir): threads kept unless `ZEN_CODEX_RESUME=0`, history injected as native items (`thread/inject_items`, fallback transcript), zenbot tools as dynamic tools. Fails loudly: refuses app-server requests it doesn't handle (ending the turn), errors quote stderr, a turn with no messages fails, `fallback_reason` reported; model listing retried until it succeeds | `turn` | `main.rs` |
| `src/faux.rs` | 102 | Scripted model `faux/smoke` (when `ZEN_FAUX=1`), steps from `ZEN_FAUX_SCRIPT` (list, or object keyed by kind of session: `verify` for a verifier, `default`): `tool`, `text`, `sleep`, `exit`; modifiers `when` (only if the prompt contains the text) and `ignore_abort` | `turn` | e2e, upgrade smoke test, evals |

Code that depends on a CLI's flags or output should fail loudly, so the daily engine update check
(`scripts/update-engines.sh`) catches a breaking CLI release.

### `zen-mind` (`packages/mind`, optional worker `pi`, ~355 lines TypeScript)

| File | Role | Notes |
|---|---|---|
| `src/main.ts` | Pi agent loop (`@earendil-works/pi-agent-core` / `pi-ai`, **pinned to 1.0.4** in `package.json`). Providers: OpenAI (ChatGPT sign-in), OpenRouter (classifiers for `s1.decide`), faux (`ZEN_FAUX=1`, model `faux/faux-1`). Kernel tools become Pi tools whose execute calls `tool.call`. Running turns keyed by `turn_id`, echoed on every message | Run from source with Node 22 type stripping: no enums, no constructor parameter properties. Must start with `node packages/mind/src/main.ts` |
| `src/credentials.ts` | File-backed credential store in the `pi-ai login` format; file mode 0600 | Reads/writes `~/.zenbot/auth.json` (secret; never print it) |

A Pi bump is a harness change: a normal commit with an eval. The daily engine job only reports a
newer Pi.

---

## Layer 3 — CLI `zen` (`crates/zen`, ~3,230 lines)

| File | Lines | Role | Depends on | Used by |
|---|---|---|---|---|
| `src/main.rs` | 826 | clap commands: `ask`, `chat`, `sessions {ls,new,show,archive,restore,rename,decide}`, `memory [--tier] [sleep]`, `models`, `login [claude\|codex\|pi]`, `status` (with a memory line), `upgrade [--check]` (the workflow commands are gone); flags `--url` (`ZEN_URL`), `--token` (`ZEN_TOKEN`), `--json`, `-c`, `-r`, `-m`, `-e`, `--inline` (`ZEN_INLINE`). Reads `~/.zenbot/env` (for `ZEN_REPO`, `ZEN_MIND_DIR`, `PATH`) and `~/.zenbot/engines.json` | `client`, `tui`, `md` | owner, scripts (`zen ask --json`), e2e, evals |
| `src/client.rs` | 232 | HTTP + WebSocket client; token from `--token`/`ZEN_TOKEN` or `~/.zenbot/token`; upgrade wait/poll messages | reqwest, tungstenite | `main.rs`, `tui.rs` |
| `src/tui.rs` | 1542 | **Largest file in the repo.** Interactive app: scrollback + live region, pickers, slash commands (`/new /resume /model /effort /done /rename /archive /upgrade /help /exit`), the `ask` tool's questions, history in `~/.zenbot/history`, banner from `~/.zenbot/version`. Render/key tests at the bottom | `client`, `editor`, `md` | `main.rs` |
| `src/editor.rs` | 391 | Multi-line input editor with prompt history | — | `tui.rs` |
| `src/md.rs` | 238 | Styled lines, word wrap, line-oriented markdown renderer | — | `tui.rs`, `main.rs` |

UI changes to `tui.rs` / `editor.rs` come with render or key tests (AGENTS.md).

## Shared: `zen-proto` (`crates/zen-proto`, 48 lines)

`text_of` (message content as text, Pi's format), `head`, `tail`. Used by `zend` (`compact`,
`agent`, `memory`, `score`), `zen-engine` (`turn`) and `zen` (`main`, `tui`). Changing how content is read
changes it in all three.

---

## Database

Postgres 16 with pgvector (`pgvector/pgvector:pg16`), user/password/db `zen` on `127.0.0.1:5432`
(`deploy/compose.yaml`). Migrations in `crates/zend/migrations/`, applied by `zend` at start.

**Migrations are expand-only** (AGENTS.md): new file per change, never edit an applied one; add
tables, columns, indexes; don't drop, rename or retype anything in the release that stops using it.
A rollback swaps binaries, not the schema, and `zend` runs with `set_ignore_missing(true)` so an
older build starts on a newer schema.

| Table | Created / altered | Purpose | Written by | Read by |
|---|---|---|---|---|
| `sessions` | 0001; `effort` 0003; `state`, `parent`, `kind` 0011; `workspace` 0013; `tainted_at` 0016; model ids rewritten 0002 | One per session (one job). `kind = 'verifier'` for child sessions; `workspace` overrides the kernel's; `tainted_at` = when it first read untrusted content (web, untrusted MCP servers); `state` from the old workflow, no longer written | `api` (create/update), `turns` (title, child sessions), `tape::append` (`updated_at`), `web`/`mcp` (`tainted_at`) | `api`, `turns`, `memory` (via `web::tainted`), e2e |
| `tape_events` | 0001; `seq`, `parent`, `hash` + functions `zen_block_hash`, `zen_rechain` 0007; index `(session_id, kind, seq)` 0014 | The tape. Kinds: `message`, `base`, `envelope`, `context`, `compaction`, `engine_session`, `questions`, `verdict`, `verification`, `taint`; from the old workflow, no longer written: `state`, `brief`, `approval`, `ruling`, `submission`, `report` | `tape::append` only (callers: `dispatch`, `turns`, `compile`, `compact`, `agent`, `web`, `mcp`) | `tape::load*`, `compact`, e2e (`tape_is_sound`) |
| `model_calls` | 0001; `turn_id`, `duration_ms` 0004; index 0012; partial index on untraced calls (`turn_id IS NULL`) 0014 | One row per assistant message: tokens, cache, cost | `dispatch` (`turn.message`) | `turns`, `measure`, `session_cost` |
| `tool_calls` | 0001; `turn_id` 0004; index 0012 | One row per tool call | `dispatch` (`tool.call`) | `turns` |
| `turns` | 0004; `envelope`, `context`, `context_tokens`, `cache_break` 0009 | One per turn: harness, worker, engine, model, effort, outcome, totals, what was sent | `turns` (`begin_turn` insert, `record_turn` update) | `api`, `measure`, `score`, `turns`, e2e, evals |
| `session_decisions` | 0005; `source` 0011 | The owner's verdict (`accept/more/reshape/drop`; ground truth); `source` was also `model` under the old workflow's auto-close | `api::decide` | e2e (scoring compares against it later) |
| `session_scores` | 0006; index `(turn_id, trigger)` 0014 | System One answers about a session | `score` | `score` |
| `envelopes` | 0008 | System prompt + tools, stored once per distinct pair, keyed by hash | `compile::envelope` | `compile` |
| `compactions` | 0010 | How each summary was made; applied ones have `applied_seq` | `compact` | `compact`, e2e |
| `decisions` | 0011 | System One decisions and what was done, by `point`: `tool` (the `decide` tool), `sleep` (the sleep's fate for each memory), `web_rerank` (search results ordered), `web_focus` (parts of a page kept); older rows from the workflow's shadow decisions | `agent::log_decision` (from `agent`, `web`), `memory::sleep` | — (for calibration) |
| `policies` | 0011 | Versioned routing policy from the old workflow | nothing | nothing (kept: expand-only; Phase 6 may reuse it) |
| `memories` | 0015 | Memory entries: text, source (`owner`, `verified`, `inferred`), tier (`short`, `long`, `archived`), use counts, the last sleep's scores, what a sleep proposed, why the tier changed | `memory` (`remember`, `sleep`) | `memory`, `api` |
| `sleep_runs` | 0015 | One row per sleep: trigger (`nightly`, `ceiling`, `owner`), scorer, counts, note, error | `memory::sleep` | `memory` (`morning_note`, `last_run`), `api` |

`scripts/db.sh pending` lists migrations the live database hasn't applied; `scripts/db.sh backup`
dumps it to `~/.zenbot/backups/` (last 10 kept).

---

## Environment variables

The service reads `~/.zenbot/env` (systemd `EnvironmentFile`). `install.sh` writes `ZEN_TOKEN`,
`ZEN_PORT`, `ZEN_REPO`, `ZEN_WORKERS`, `ZEN_MIND_DIR`, `HOME`, `PATH` there. Workers inherit
the kernel's environment.

### Kernel (`zend`)

| Var | Default | Read in | Effect |
|---|---|---|---|
| `ZEN_TOKEN` | required | `main.rs` | API token; also masked in tool output |
| `DATABASE_URL` | `postgres://zen:zen@127.0.0.1:5432/zen` | `main.rs`, `scripts/db.sh` | Database |
| `ZEN_PORT` | `8100` | `main.rs`, scripts | Listen port (0.0.0.0) |
| `ZEN_WORKSPACE` | `$HOME` | `main.rs` | Default working directory for tools |
| `ZEN_REPO` | `$HOME/zenbot` | `main.rs`, `zen` CLI | zenbot's checkout: named in the system prompt; used by the updater |
| `ZEN_DEFAULT_MODEL` | `claude/claude-opus-5-5` | `main.rs` | Model for new sessions |
| `ZEN_HARNESS` | `~/.zenbot/version` | `main.rs` | Build id recorded with every turn (dev, smoke and eval kernels set it) |
| `ZEN_WORKERS` | `engine` (+`pi` if `$ZEN_MIND_DIR/node_modules` exists) | `workers.rs`, scripts | Workers to start |
| `ZEN_ENGINE_CMD` | `zen-engine` next to `zend` | `workers.rs` | Command for `engine` |
| `ZEN_MIND_CMD` | `node src/main.ts` | `workers.rs` | Command for `pi` |
| `ZEN_MIND_DIR` | `packages/mind` (relative to the service's cwd) | `workers.rs`, `zen login pi` | Pi worker directory |
| `ZEN_WORKER_<NAME>_CMD` | the name | `workers.rs` | Command for any other worker name |
| `ZEN_MODELS` | `workers::DEFAULT_MODELS` | `workers.rs` | Curated model list and order |
| `ZEN_HOME` | `$HOME/.zenbot` | `main.rs` | Prompt files, skills, `MEMORY.md`, `mcp.json`, web PDFs and long MCP output in `outputs/` (dev, eval and smoke kernels set their own) |
| `ZEN_SOUL_CHARS` / `ZEN_AGENTS_CHARS` / `ZEN_USER_CHARS` | `4000` / `12000` / `3000` | `compile.rs` | Size cap of each prompt file in the instructions |
| `ZEN_SKILLS_DIR` | `$ZEN_HOME/skills` | `skills.rs` | Where skills live |
| `ZEN_SKILL_INDEX_CHARS` | `2500` | `skills.rs` | Above this the skills index lists domains only |
| `ZEN_MEMORY_CHARS` | `4000` | `memory.rs` | Short-term memory's size; twice it is the hard ceiling |
| `ZEN_MEMORY_PROMOTE` | `shadow` | `memory.rs` | `on`: the sleep moves memories to long-term instead of proposing it |
| `ZEN_MEMORY_PROMOTE_BAR` | `0.95` | `memory.rs` | Durable and impactful bar for long-term, on the lowest of three samples |
| `ZEN_DECIDE_TOOL` | on | `agent.rs` | `0` hides the `decide` tool |
| `ZEN_S1_MODEL` | unset (off) | `score.rs` | System One classifier: scoring, `decide`, the sleep |
| `ZEN_S1_PRIVATE` | on | `score.rs` | `0`: System One sees only the conversation, not private content (today: memories in the sleep, and page text for `web_fetch`'s `focus`) |
| `ZEN_SEARCH_PROVIDER` | `brave` if `BRAVE_API_KEY` is set, else `tavily` if `TAVILY_API_KEY` is, else `searxng` | `web.rs` | Which search provider `web_search` uses |
| `BRAVE_API_KEY` / `TAVILY_API_KEY` | unset | `web.rs` | Keys for Brave / Tavily search (secret-looking, so masked in tool output) |
| `ZEN_SEARXNG_URL` | `http://127.0.0.1:8888` | `web.rs` | SearXNG base URL: the keyless provider and the rescue for a failed keyed call |
| `ZEN_SEARCH_RERANK` | on | `web.rs` | `0`: don't have System One rerank search results |
| `ZEN_MCP_CONFIG` | `$ZEN_HOME/mcp.json` | `mcp.rs` | MCP servers file; `${VAR}` in it filled from the kernel's environment |
| `ZEN_SCORE_IDLE_SECS` | `7200` | `score.rs` | Idle time before a session is scored |
| `ZEN_CONTEXT_TOKENS` | `200000` | `compact.rs` | Context budget (capped by the model's window) |
| `ZEN_COMPACT_SOFT` / `_HARD` / `_KEEP` | `0.7` / `0.9` / `0.3` | `compact.rs` | Fractions of the budget: prepare a summary / apply now / keep verbatim |
| `ZEN_COMPACT_IDLE_SECS` | `300` | `compact.rs` | Pause after which a prepared summary is applied |
| `ZEN_SUMMARY_MODEL` | `claude/claude-sonnet-5-5` | `compact.rs` | Summarizer |
| `ZEN_TURN_IDLE_SECS` | `600` | `turns.rs` | Watchdog: abort a quiet turn |
| `ZEN_TURN_ABORT_GRACE_SECS` | `30` | `turns.rs` | Watchdog: end a turn in the kernel this long after an abort |
| `ZEN_UPDATE_CHECK_SECS` | `3600` | `update.rs` | How often to compare with `origin/main` (`0` off) |
| `RUST_LOG` | `zend=info` | `main.rs` | Log filter |
| `HOME` | `/tmp` | many | Location of `~/.zenbot` |

Set by the kernel for every `bash` command: `ZEN_SESSION_ID`, `ZEN_MODEL` (read by
`scripts/git-hooks/prepare-commit-msg` to add commit trailers).

### Workers

| Var | Default | Read in | Effect |
|---|---|---|---|
| `ZEN_FAUX` | off | `faux.rs`, `mind/src/main.ts` | `1` lists the scripted models (`faux/smoke`, Pi's `faux/faux-1`) |
| `ZEN_FAUX_SCRIPT` | built-in script | `faux.rs` | JSON file of faux steps |
| `ZEN_CLAUDE_RESUME` | on | `claude.rs` | `0`: every turn in a fresh, unsaved Claude Code session |
| `ZEN_CODEX_RESUME` | on | `codex.rs` | Keep Codex threads across turns (Codex ties its prompt cache to the thread); `0`: a new thread per turn |
| `ZEN_CODEX_INJECT` | on | `codex.rs` | `0`: history as a transcript instead of native items |
| `CLAUDE_CONFIG_DIR` | `~/.claude` | `claude.rs` | Where Claude Code keeps its sessions |
| `ZEN_AUTH_FILE` | `~/.zenbot/auth.json` | `mind/src/main.ts` | Pi's sign-in file (secret) |
| `OPENROUTER_API_KEY` | unset | pi-ai library (not read in our code) | Enables OpenRouter classifiers in the Pi worker |

### CLI and scripts

| Var | Default | Read in | Effect |
|---|---|---|---|
| `ZEN_URL` | `http://127.0.0.1:8100` | `zen` | Kernel URL |
| `ZEN_TOKEN` | `~/.zenbot/token` | `zen` | API token |
| `ZEN_INLINE` | off | `zen` | Inline terminal app |
| `ZEN_SMOKE_PORT` | `18199` | `upgrade.sh` | Smoke-test kernel port |
| `ZEN_DEV_PORT` / `ZEN_DEV_DB` / `ZEN_DEV_HOME` | `18100` / `zen_dev` / `~/.zenbot/dev` | `dev.sh` | Dev kernel port, database and zenbot home |
| `ZEN_E2E_PORT` / `ZEN_E2E_KEEP` / `ZEN_E2E_NO_BUILD` | `18377` / – / – | `e2e.sh` | e2e kernel port (the test MCP and SearXNG servers use the next two ports); keep DB and files; skip the build (CI) |
| `ZEN_SLOW_SECS` | `12` | `scripts/e2e/slow_worker.py` | How long the test summarizer's `complete` takes |
| `ZEN_EVAL_PORT` / `ZEN_EVAL_KEEP_BUILDS` | `18301` / `5` | `eval.sh` | Eval kernel port; base builds kept |
| `ZEN_ENGINES` / `ZEN_ENGINES_FAIL` / `ZEN_ENGINES_<ENGINE>_MODEL` | all / – / cheapest listed | `update-engines.sh` | Limit engines; force a failed check (tests rollback); model for the check |
| `ZEN_BUILD_FROM_SOURCE` | – | `fetch-release.sh` (so `install.sh`, `upgrade.sh`) | `1`: never download binaries |
| `ZEN_RELEASE_BASE` | GitHub `edge` release of `origin` | `fetch-release.sh` | Where binaries are downloaded from |

---

## `~/.zenbot/` (config and state on the host)

| Path | Written by | Read by | Notes |
|---|---|---|---|
| `env` | `install.sh` | systemd (`EnvironmentFile`), `zen` CLI, scripts (`lib.sh::zen_env`) | Service environment; mode 600 |
| `token` | `lib.sh::new_token` (install, dev) | `zen` CLI, `install.sh` (copies into `env`), `secrets.rs`, scripts | **Secret.** API token |
| `auth.json` | `zen login pi` (pi-ai CLI), `credentials.ts` | Pi worker, `secrets.rs` | **Secret. Never print it.** Pi's ChatGPT sign-in |
| `version` | `install.sh`, `apply-upgrade.sh` | `update.rs`, `zen` banner | Installed commit |
| `upgrade.log` | `apply-upgrade.sh`, `update-engines.sh` | owner, `update.rs` (last line), `zen upgrade` | Upgrade and engine-update results |
| `engines.json` | `update-engines.sh` | `zen status` | Engine versions from the last check |
| `history` | `zen` TUI | `zen` TUI | Prompt history |
| `backups/` | `db.sh backup` (before pending migrations) | owner | Last 10 kept |
| `bin/` | `install.sh`, `apply-upgrade.sh` | systemd, `~/.local/bin/zen` link | `zend`, `zen`, `zen-engine` (+ `.prev` for rollback) |
| `outputs/` | `tools.rs`, `web.rs`, `mcp.rs` | the model (`read`, `bash`) | Full output of cut tool results; PDFs from `web_fetch` (`web-*.pdf`); MCP output over 50 KB (`mcp-*.txt`). `tools.rs` writes under `$HOME/.zenbot`, `web`/`mcp` under `ZEN_HOME` |
| `engine/claude/`, `engine/complete/` | `claude.rs` | Claude Code | Fixed directories for engine sessions and completions |
| `SOUL.md`, `AGENTS.md`, `USER.md` | `defaults.rs` when missing, then the owner | `compile.rs` | Prompt files: who the agent is, its environment, the owner |
| `MEMORY.md` | `memory::export` | the owner | Copy of short-term memory (edit with `remember`, not here) |
| `skills/<domain>/<name>/` | `defaults.rs` (`work/brief`, `work/verify`) when missing, then the agent and the owner | `skills.rs` | Skills |
| `mcp.json` | the owner | `mcp.rs` (re-read when it changes) | MCP servers (`mcpServers`); keep secrets in `env` and refer to them as `${VAR}` |
| `dev/` | `scripts/dev.sh` | the dev kernel | The dev kernel's own `ZEN_HOME` (`ZEN_DEV_HOME`) |
| `evals/<run>/` | `eval.sh` | `eval-report.sh` | Eval results |

Claude Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`.

---

## Scripts, install, deploy, CI

| Path | Does | Touches |
|---|---|---|
| `install.sh` | Fresh-VM install (safe to re-run): apt packages (git, curl, jq, bubblewrap, docker), Node 22 in `~/.local/node`, Claude Code and Codex CLIs, `npm ci` for Pi if enabled, binaries (download or build), `~/.zenbot/{bin,env,token,version}`, systemd units, git hooks path | system, `~/.zenbot`, `/etc/systemd/system` |
| `scripts/upgrade.sh` | Build (or fetch) → `cargo test` (local builds) → ping workers → smoke turn per worker on a second kernel (`:18199`, throwaway copy of the live DB, so migrations are tried there; its own `ZEN_HOME` in the smoke workspace) → schedule `apply-upgrade.sh` via `systemd-run`. `--check` stops before scheduling | `target/`, temp DB `zen_smoke_*` |
| `scripts/apply-upgrade.sh` | Detached: wait for `"busy":0` (up to 30 min, then goes ahead), back up DB if migrations are pending, swap binaries (keeps `.prev`), write `version`, restart, health check, roll back if unhealthy; when healthy, `install_timers` and `docker compose … up -d` (so new timers and compose services such as SearXNG arrive with an upgrade) | `~/.zenbot/{bin,version,upgrade.log,backups}`, service |
| `scripts/self-update.sh` | `git pull` main then `upgrade.sh`; refuses a dirty checkout or another branch. Used by `zen upgrade`, `/upgrade`, `POST /api/upgrade` | checkout |
| `scripts/fetch-release.sh` | Download CI's binaries for HEAD into `target/release` (checksum verified); fails (changing nothing) on local changes under `crates/`, `Cargo.*`, non-x86_64-Linux, or no build. `--check REF` only checks | `target/release` |
| `scripts/update-engines.sh` | Daily (timer): update Claude Code / Codex CLIs, test with a real `complete` through `zen-engine`, roll back on failure; Pi only reported. `--check` reports only | CLI installs, `upgrade.log`, `engines.json` |
| `scripts/sleep.sh` | Nightly (timer): waits for a healthy kernel, `POST /api/memory/sleep?trigger=nightly`, prints the counts | live kernel |
| `scripts/db.sh` | DB helpers run inside the Postgres container: `pending`, `backup`, copy/drop/restore helpers | live DB, `~/.zenbot/backups` |
| `scripts/lib.sh` | `zen_env`, `pi_enabled`, `wait_healthy`, `ensure_rust`, `new_token`, `install_timers` (writes and enables the `deploy/` timers; `install.sh` and, after a healthy upgrade, `apply-upgrade.sh`) | `/etc/systemd/system` |
| `scripts/dev.sh` | Dev kernel in the foreground on `:18100` with database `zen_dev` and `ZEN_HOME` `~/.zenbot/dev`, using `~/.zenbot/env` settings | `zen_dev` DB, `~/.zenbot/dev` |
| `scripts/e2e.sh` | End-to-end scenarios (below); scripts in `scripts/e2e/*.json`, plus test servers in Python: `slow_worker.py` (a worker serving `slow/summarizer`, a deliberately slow `complete`), `mcp_server.py` (an MCP server with `echo` and `add`, stdio or `--http PORT`), `searxng_stub.py` (answers `/search?format=json` with fixed results) | temp DB, workspace and `HOME` |
| `scripts/eval.sh`, `scripts/eval-report.sh` | Harness eval: this checkout vs installed (or `--base REF`), same model; each kernel gets its own `ZEN_HOME` next to the task workspace (default prompt files and skills); report for the owner, never a gate | `zen_eval_*` DBs, `~/.zenbot/evals/` |
| `scripts/git-hooks/prepare-commit-msg` | Adds `Zen-Session` / `Co-Authored-By` trailers when `ZEN_SESSION_ID` is set | commit messages |
| `deploy/compose.yaml` | `postgres` (pgvector, `127.0.0.1:5432`) and `searxng` (`searxng/searxng:latest`, `127.0.0.1:8888`, keyless search for `web_search`) | Docker volume `zen-pg` |
| `deploy/searxng/settings.yml` | SearXNG settings mounted read-only: JSON output on, limiter and image proxy off, not a public instance | — |
| `deploy/zenbot.service` | Runs `~/.zenbot/bin/zend` with `EnvironmentFile=~/.zenbot/env`; `ExecStartPre` brings Postgres up (must succeed) and SearXNG (a failure is ignored); `Restart=always` | — |
| `deploy/zen-engines.service` / `.timer` | `update-engines.sh` daily at 04:00 UTC (+ up to 1 h random delay) | — |
| `deploy/zen-sleep.service` / `.timer` | `sleep.sh` nightly at 03:00 UTC (+ up to 30 min random delay) | — |
| `.github/workflows/ci.yml` | On PRs and pushes to main: build, `cargo test`, clippy `-D warnings`, `scripts/e2e.sh`. On main, if green: build `dist` profile, publish `zenbot-x86_64-linux-<sha12>.tar.gz` to the rolling `edge` release (20 newest kept) | GitHub releases |

---

## Tests

- **Unit tests** live in `#[cfg(test)] mod tests` at the bottom of each file: `zend` (`tools` 10,
  `memory` 5, `skills` 5, `web` 5, `compile` 4, `mcp` 3, `compact` 3, `score` 3, `agent` 2, `context` 2, `measure` 2,
  `secrets` 2, `defaults` 1, `api` 1, `git` 1), `zen` (`tui` 16 render/key tests, `editor` 5,
  `main` 2, `client` 1), `zen-engine`
  (`claude` 3, `codex` 2, `turn` 2), `zen-proto` 1. Run `cargo test --release`.
- **End to end:** `scripts/e2e.sh [filter]` builds, then runs each scenario on a fresh kernel (port
  18377, `ZEN_FAUX=1`, `ZEN_WORKERS=engine`) with its own git workspace, all on one throwaway
  database (`zen_e2e_<pid>`), checking the database. At the end it checks every tape is numbered
  without gaps and its hash chain recomputes. Scenarios (14): `open-loop`, `restart-recovery` (an old session in a workflow state still
  works), `prompt-files` (defaults installed, the owner's kept, all in the instructions, the tool
  list), `skills` (found and loaded on demand, nothing outside a skill), `mcp` (find, load and call
  tools on a stdio and an HTTP test server, a missing argument refused, remote output wrapped and
  the session tainted), `web` (loopback and metadata addresses refused, search through a SearXNG
  stub, one envelope with markers defused, a tainted session's memory saved as `inferred`),
  `memory-across-sessions`,
  `memory-sleep` (the ceiling, a sleep tidies to size, archives, records, the morning note), `ask`
  (ends the turn; `move` is gone), `verifier` (commands by the kernel, a read-only verifier,
  a failed command skips it), `summaries`, `secrets`, `slow-summary` (a
  summary at the hard limit slower than the watchdog: the turn waits, then runs), `stale-turn` (a
  turn the kernel ended keeps running in the worker; its late answer must not reach the next turn).
  Faux scripts in `scripts/e2e/*.json` (`reach.json` drives `mcp` and `web`); `slow-summary` adds the
  `slow` worker from `scripts/e2e/slow_worker.py` via `ZEN_WORKER_SLOW_CMD`; `mcp` starts
  `scripts/e2e/mcp_server.py` (stdio, and `--http` on port +1), `web` starts
  `scripts/e2e/searxng_stub.py` on port +2 (`ZEN_SEARXNG_URL`). Nothing reaches the internet. Add a scenario when kernel behavior changes.
- **Evals:** `evals/tasks/<name>/{task.json,files/}` (13 tasks, including the runner self-test
  `smoke`), run by `scripts/eval.sh`; see `evals/README.md`. Harness changes (system prompt,
  history, tools, workers, model or effort handling) get an eval before commit; the report goes to
  the owner, who decides.
- **Upgrade smoke test:** one scripted turn per worker inside `scripts/upgrade.sh`.

---

## Notes and known gaps

- **Columns and tables of the old workflow stay** (expand-only): `sessions.state`, `policies`, and
  old tape blocks. Nothing writes them, but two readers remain: `api::list_sessions` still returns
  `state` (`"open"` when null), and `compile::history` still honours an old `state` block with
  `fresh: true` (history starts after it), so old sessions replay as before. Drop them in a later
  release once nothing needs them.
- **Criteria commands run unsandboxed.** `verify` runs a criterion's `run` command on the bash
  tool's core (`tools::run_shell`: own process group, 600 s timeout, output masked) with
  `read_only = false`, so outside bubblewrap and able to write, even though the verifier's own
  shell is read-only.
- **Workers start through a login shell** (`bash -lc` in `mind.rs`), so the service user's
  profile can change their environment.
- Instruction files attached to tool results (`dispatch.rs`) are appended after masking, so they
  are not masked.
- **`codex.rs` header says threads are ephemeral**, but the code keeps them across turns unless
  `ZEN_CODEX_RESUME=0`.
- **`GET /api/mcp` connects every configured server** (starts stdio ones) to count their tools.
  Its doc comment says it is for `zen status`, but the CLI doesn't call it yet.
- **`compile::turn_context` still takes a workflow `phase`**; the only production caller
  (`turns.rs`) passes `None`.
- **Skills aren't in git yet**: `~/.zenbot/skills` is a plain folder (versioning with revertible
  commits comes with self-improving skills, Phase 5).
- The protocol as built is in `docs/worker-protocol.md`; any sketch of it in `SPEC.md` is a target,
  not the code.
- **MCP stdio servers** start with a clean environment (PATH, HOME, USER, LANG, LC_ALL, TZ, TMPDIR and
  the config's `env`), so the kernel's token and keys never reach them; they still run outside
  bubblewrap, and remote MCP URLs aren't address-checked like `web_fetch` (the owner configures them).

## Deeper docs

| Doc | What's there |
|---|---|
| `AGENTS.md` | How to change zenbot: build, test, upgrade, conventions, rules. Read it fully first |
| `CONTEXT.md` | What zenbot is for, how success is measured, constraints. Read first |
| `DESIGN.md` | How the system works today, then the target design agreed on 2026-10-06 |
| `DEVELOPMENT.md` | Local loop: build, test, verify and ship a change |
| `ROADMAP.md` | Phases in order |
| `PROGRESS.md` | Append-only log of what shipped and what was learned |
| `DECISIONS.md` | Why things are the way they are (D-025 onward: the target design and the choices made building it) |
| `SPEC.md` | Long-term target modules (section numbers kept, e.g. 5.18 Security) |
| `docs/context.md` | How each turn's context is built, stored, cached, summarized and measured; the tape |
| `docs/brief.md` | Briefs and verification: the `work/brief` and `work/verify` skills, the `verify` and `ask` tools |
| `docs/worker-protocol.md` | Kernel ⇄ worker JSON-RPC: methods, messages, engine sessions, guarantees |
| `docs/client-protocol.md` | HTTP API and WebSocket events for clients |
| `evals/README.md` | Eval tasks and the report |
| `README.md`, `INSTALL.md` | Using and installing zenbot |
