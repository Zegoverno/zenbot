# zenbot — Codebase Map

> **Purpose:** a blast-radius guide for coding agents. Before touching anything, find it here to see
> what it does, what it depends on and what depends on it. The map is not the territory: read the
> code before changing it, and if the two disagree, the code wins (then fix the map).
>
> Written from the code on 2026-10-06 and updated after PRs #8, #9, #10 and #13. Where something couldn't be confirmed in the code it says
> "unverified". Line counts are approximate and only show which files are big.

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
`use super::*`. ~5,100 lines of Rust plus migrations, step prompts and the web UI.

### Files

| File | Lines | Role | Main items | Depends on | Used by |
|---|---|---|---|---|---|
| `src/main.rs` | 234 | Startup and shared state. Reads env, connects to Postgres, runs migrations (`set_ignore_missing(true)` so a rolled-back build starts on a newer schema), repairs the tape, spawns workers, sends sessions stuck in `verifying` back to `working`, starts background tasks, builds the router | `App` (incl. `hubs`, `state_changes`), `subscribe`/`unsubscribe`/`emit` (a session's event hub exists only while a client is connected; `emit` with no client sends nothing), `env_num`, `load_messages`, `context_in`, `append_tape` | every module | everything (via `App`) |
| `src/api.rs` | 365 | HTTP handlers and the WebSocket (docs/client-protocol.md). Token auth middleware (header `Authorization: Bearer` or `?token=`, compared in constant time). A WebSocket `prompt` starts its turn in a separate task, so an `abort` is read while the turn is prepared | `auth`, `same_secret`, `health`, `version`, `upgrade_*`, `list_models`, `*_session`, `flow_action`, `decide`, `DECISIONS`, `handle_socket` | `turns` (start/abort), `flow`, `score`, `workers`, `update` | router in `main.rs` |
| `src/turns.rs` | 611 | A turn's lifecycle: start (owner or kernel), compile what it sends, record it in `turns`, finish, abort, watchdog, child sessions (verifiers). `Turn.state` holds the workflow state the turn gates tools by (read once at start, re-read if `state_changes` moved, kept current by `flow::set_state`). A summary made inline at the hard limit is `hold`-counted as a running tool so the watchdog waits, and an abort stops it. `record_turn` returns the row plus the session's parent and cost in one query | `Turn`, `Origin`, `Recorded`, `start_turn`, `start_kernel_turn`, `begin_turn`, `still_ours`, `hold`, `finish_turn`, `abort_turn`, `run_child`, `watchdog`, `record_turn`, `session_cost` | `compile`, `compact`, `measure`, `flow`, `tape`, `tools`, `workers` | `api`, `dispatch`, `flow`, `workers` |
| `src/dispatch.rs` | 203 | Handles every message from workers: `tool.call` (gated by `flow::refuse`, then `flow::run_tool`, `history`, or `tools::execute`), `turn.delta`/`turn.thinking`, `turn.message` (tape + `model_calls`), `turn.usage`, `turn.end`. Drops messages not from the turn running on that worker (matched by echoed `turn_id`; by session and worker when a message has none). Reads the state from `Turn.state`, not the DB. Attaches newly met AGENTS.md files to tool results | `dispatch`, `handle_incoming`, `touch`, `turn_id` | `flow`, `tools`, `compact`, `context`, `turns` | `main.rs` (spawned task) |
| `src/flow.rs` | 1043 | **Largest kernel file.** Briefed work (docs/brief.md): session states, which tools exist and may run per state, the brief schema, approval, verification (kernel runs criteria commands, a child verifier session judges the rest), report, auto-close, System One shadow decisions, routing policy | `mode`, `initial_state`, `state`, `set_state`, `tools_for`, `allowed`, `refuse`, `system_for`, `phase_line`, `validate_brief`, `render_brief`, `run_tool`, `approve`, `verify`, `report`, `after_turn` | `tape`, `tools` (`run_shell` for criteria checks), `git`, `secrets`, `score`, `workers`, `turns`; `steps/*.md` | `api`, `dispatch`, `turns`, `main.rs` |
| `src/tools.rs` | 728 | The built-in tools and their execution: `bash` on `run_shell` (own process group killed on timeout or abort, read-only `bwrap` sandbox in read-only states; also used by `flow::run_check`), `read`, `write`, `edit` (whitespace/line-ending normalized matching), `move`. Per-file locks. Output over 50 KB cut to head and tail (off the runtime threads), full text saved under `~/.zenbot/outputs/`. Every result goes through `secrets::mask` | `specs`, `execute`, `run_shell`, `Shell`, `resolve`, `ToolOutput` | `compact` (history spec), `secrets` | `dispatch`, `flow`, `turns`, `context` |
| `src/compile.rs` | 240 | What a turn sends, in cache order: envelope (system prompt + tools, stored once in `envelopes`) → summary → history → prompt with turn context | `system_prompt`, `base_prompt`, `envelope`, `turn_context`, `history`, `Envelope`, `SummaryRef` | `context`, `tape` | `turns`, `measure` |
| `src/context.rs` | 150 | Instruction files (AGENTS.md, else CLAUDE.md, capped at 32 KB): always `~/.zenbot/AGENTS.md` and one per directory from `/` to the workspace; on demand, a project's file the first time a tool touches a path in it (tape kind `context`) | `always`, `governing`, `paths_in_call`, `attachment`, `read_capped` | `tools::resolve` | `compile`, `dispatch` |
| `src/compact.rs` | 460 | Summaries of older turns: prepared in the background past the soft limit, applied after a pause or at once past the hard limit (`compaction` block). Also the `history` tool, which reads old blocks back by number or search | `Settings`, `summary_model`, `plan`, `prepare`, `pending`, `apply`, `tool_spec`, `history_tool` | `tape`, `workers::complete` | `turns`, `dispatch`, `tools` |
| `src/measure.rs` | 177 | Per-turn record of what was sent and why the prompt cache could or couldn't be reused (`cache_break`: `first`, `instructions`, `summary`, `model`, `engine_session`, `expired`; unexpected: `history`, `miss`) | `previous`, `record`, `break_at_start`, `break_at_end`, `cache_ttl_secs` | `compile` types | `turns` |
| `src/tape.rs` | 70 | The tape: append (advisory lock per session, `seq` + parent + hash computed in SQL by `zen_block_hash`), load, repair (`zen_rechain`) | `Block`, `append`, `load`, `load_all`, `repair` | DB functions from migration 0007 | `compile`, `compact`, `flow`, `turns`, `main.rs` |
| `src/workers.rs` | 189 | Worker configs from `ZEN_WORKERS`, supervision with backoff (ends orphaned turns on a crash), model and classifier routing, curated model list, effort checks, `complete` for summaries | `Worker`, `worker_configs`, `supervise`, `complete`, `DEFAULT_MODELS`, `collect_models`, `worker_for`, `model_info`, `check_effort` | `mind`, `score` | `main.rs`, `api`, `turns`, `flow`, `compact` |
| `src/mind.rs` | 124 | JSON-RPC client for one worker process (`bash -lc <cmd>`); 30 s default request timeout, `request_within` for longer | `Mind`, `Incoming`, `spawn`, `request`, `request_within`, `respond` | — | `workers`, `main.rs` |
| `src/score.rs` | 266 | Live scoring: a System One classifier answers fixed, versioned questions (`QUESTIONS_VERSION = "v1"`) about a session after a decision or after it goes idle; stored in `session_scores`. Off unless `ZEN_S1_MODEL` is set | `scorer`, `questions`, `state`, `score_session`, `idle_loop` | `workers` (`s1.decide`) | `api`, `flow`, `workers`, `main.rs` |
| `src/secrets.rs` | 193 | Masks secrets in tool output: values of the kernel's own secret-looking env vars, `~/.zenbot/token`, `~/.zenbot/auth.json` values, and well-known token prefixes / private key blocks | `mask`, `mask_off_thread` (large text on a blocking thread) | — | `tools`, `flow` (check output, diff) |
| `src/update.rs` | 176 | Self-update: compares `~/.zenbot/version` with `origin/main` (hourly by default), asks `scripts/fetch-release.sh --check` whether binaries exist, starts `scripts/self-update.sh` on request | `Updater` (`running`, `info`, `check`, `check_periodically`, `start`, `status`) | `git`, scripts | `api`, `main.rs` |
| `src/git.rs` | 64 | Async git with a 60 s limit and an output cap (git is stopped once the cap is reached) | `output`, `git` | — | `update`, `flow` (diff baseline, `diff_since`) |
| `steps/frame.md`, `steps/work.md`, `steps/verify.md` | ~25 | Procedures the model is told in briefed work (`include_str!` in `flow.rs`); `verify.md` is the verifier's whole system prompt | — | — | `flow::system_for` |
| `web/index.html` | 493 | Browser UI, served at `/` (`include_str!`). **Frozen** (AGENTS.md). Uses `/api/models` and `/api/sessions…` | — | API | owner |
| `migrations/*.sql` | — | Schema (see Database) | — | — | `sqlx::migrate!` in `main.rs` |

### Invariants stated in header comments

- `main.rs`: "Owns all state and all side effects."
- `tools.rs`: "The kernel is the only place side effects happen." Tool specs are in a fixed order
  because they are part of the cached prefix.
- `compile.rs` / `docs/context.md`: instructions and tools are fixed for the session; history is
  append-only; anything that changes per turn goes at the end (turn context). Breaking this breaks
  the prompt cache (`measure.rs` will report `history` or `miss`).
- `flow.rs`: the gates are enforced in code, not prompts: which tools may run per state, the brief's
  schema, work only after approval, the criteria checks. **One tool list for every phase** (except
  `open` and the verifier) so the cache holds across phases; what may run is checked at call time.
- `tape.rs`: appends to one session are serialized by an advisory lock; the hash is computed by the
  database so every writer hashes the same canonical JSON.
- `dispatch.rs`: the worker gets its answer before the tool call is recorded, so a DB failure can't
  leave the model waiting. Messages from a turn other than the one running on that worker (by
  `turn_id`) are dropped, so a turn the kernel already ended can't leak into the next one.
- `flow::set_state` changes the state under the turns lock and bumps `App.state_changes`, so a turn
  starting at the same moment can't miss it (`begin_turn` re-reads when the count moved).
- `secrets.rs`: masking applies to what tools return; the command text the model typed is not masked.

---

## Kernel routes

From `main.rs` (router) and `api.rs`. Everything under `/api` needs the token.

| Method | Path | Handler | Does |
|---|---|---|---|
| GET | `/` | `index` | Web UI (no auth; the page asks for the token) |
| GET | `/health` | `health` | No auth. `{ok, db, mind, workers:{name:bool}, busy, version, commit}`; pings every worker. `busy` = running turns + background workflow steps. Used by `wait_healthy` and `apply-upgrade.sh` (`"busy":0`) |
| GET | `/api/models` | `list_models` | Asks every worker for `models.list`, refreshes routes, returns the curated list (`ZEN_MODELS` order, plus any `faux/*`), `authenticated`, `default`, `scorer` |
| GET | `/api/sessions?archived=` | `list_sessions` | Top-level sessions only (`kind IS NULL`, so verifiers are hidden), with cost |
| POST | `/api/sessions` | `create_session` | `{title?, model?, effort?}`; state = `flow::initial_state()` |
| GET | `/api/sessions/{id}` | `get_session` | Session, its `message` blocks (with `seq`) and `busy` |
| PATCH | `/api/sessions/{id}` | `update_session` | Title, model, effort (`"default"` clears it), archived |
| GET | `/api/sessions/{id}/ws` | `session_ws` | WebSocket; its session's event hub is created on connect and freed when the last client leaves. Client sends `{type:"prompt",text}` or `{type:"abort"}`. Server events: `message`, `delta`, `thinking`, `tool_start`, `tool_end`, `busy`, `end`, `idle`, `state`, `status`, `brief`, `questions`, `report`, `child_end`, `error`, `resync` |
| POST | `/api/sessions/{id}/decision` | `decide` | `{decision: accept\|more\|reshape\|drop, note?}` → `session_decisions`; moves briefed work on (`more`→working, `reshape`→framing, else closed); triggers scoring |
| POST | `/api/sessions/{id}/flow` | `flow_action` | `{action: brief\|quick\|go\|verify}`; 409 while the session is busy |
| GET | `/api/version?refresh=` | `version` | Running commit vs `origin/main` |
| GET/POST | `/api/upgrade` | `upgrade_status` / `upgrade_start` | Start `scripts/self-update.sh`; status = job + last line of `~/.zenbot/upgrade.log` |

---

## Tools the model gets

Built in `tools.rs::specs()` (fixed order: `bash`, `read`, `write`, `edit`, `move`, then `history`
from `compact.rs`) and extended per state by `flow.rs::tools_for()`. With `ZEN_BRIEFS=off`,
`turns.rs` sends `tools::specs()` only and nothing is refused.

| Tool | Executed by | Offered in | May run in |
|---|---|---|---|
| `bash` | `tools.rs` (bwrap read-only sandbox in `framing` and `verifier`) | all | all |
| `read` | `tools.rs` | all | all |
| `write`, `edit`, `move` | `tools.rs` | all but verifier | `open`, `working` |
| `history` | `compact.rs::history_tool` | all but verifier | `open`, `framing`, `working` |
| `propose_brief` | `flow.rs` (validates, `brief` block; from `open` moves to `framing`) | all but verifier | `open`, `framing` |
| `ask` | `flow.rs` (`questions` block, ends the step) | all non-open, non-verifier | `framing`, `working`, `open`* |
| `note_ruling` | `flow.rs` (`ruling` block) | all non-open, non-verifier | `working`, `open`* |
| `submit_work` | `flow.rs` (`submission` block; kernel then verifies) | all non-open, non-verifier | `working` |
| `submit_verdict` | `flow.rs` (`verdict` block) | verifier only | verifier |
| `decide` | `flow.rs` → worker `s1.decide`, logged in `decisions` | when `ZEN_S1_MODEL` is set and `ZEN_DECIDE_TOOL` isn't `0` | `open`, `framing`, `working` |

\* `allowed()` lets `open` run anything except `submit_work` and `submit_verdict`, but an `open`
session is only offered the base tools, `propose_brief` and `decide`.

**Session states** (`sessions.state`, also `state` blocks on the tape): `open` (default with
`ZEN_BRIEFS=opt-in`), `framing` (read-only), `working`, `verifying`, `reported`, `closed`, and
`verifier` for child sessions. A tool that ends a step (`ask`, `propose_brief`, `submit_work`,
`submit_verdict`) makes the kernel refuse further calls in that turn. `flow::after_turn` decides what
happens next (auto-approve, wait for approval or answers, start verification).

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
| `src/faux.rs` | 120 | Scripted model `faux/smoke` (when `ZEN_FAUX=1`), steps from `ZEN_FAUX_SCRIPT` (list, or object keyed by phase: `frame`, `work`, `verify`, `default`): `tool`, `text`, `sleep`, `exit`; modifiers `when` (only if the prompt contains the text) and `ignore_abort` | `turn` | e2e, upgrade smoke test, evals |

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

## Layer 3 — CLI `zen` (`crates/zen`, ~3,220 lines)

| File | Lines | Role | Depends on | Used by |
|---|---|---|---|---|
| `src/main.rs` | 762 | clap commands: `ask`, `chat`, `sessions {ls,new,show,archive,restore,rename,decide,flow}`, `models`, `login [claude\|codex\|pi]`, `status`, `upgrade [--check]`; flags `--url` (`ZEN_URL`), `--token` (`ZEN_TOKEN`), `--json`, `-c`, `-r`, `-m`, `-e`, `--inline` (`ZEN_INLINE`). Reads `~/.zenbot/env` (for `ZEN_REPO`, `ZEN_MIND_DIR`, `PATH`) and `~/.zenbot/engines.json` | `client`, `tui`, `md` | owner, scripts (`zen ask --json`), e2e, evals |
| `src/client.rs` | 232 | HTTP + WebSocket client; token from `--token`/`ZEN_TOKEN` or `~/.zenbot/token`; upgrade wait/poll messages | reqwest, tungstenite | `main.rs`, `tui.rs` |
| `src/tui.rs` | 1596 | **Largest file in the repo.** Interactive app: scrollback + live region, pickers, slash commands (`/new /resume /model /effort /done /go /brief /quick /verify /rename /archive /upgrade /help /exit`), history in `~/.zenbot/history`, banner from `~/.zenbot/version`. Render/key tests at the bottom | `client`, `editor`, `md` | `main.rs` |
| `src/editor.rs` | 391 | Multi-line input editor with prompt history | — | `tui.rs` |
| `src/md.rs` | 238 | Styled lines, word wrap, line-oriented markdown renderer | — | `tui.rs`, `main.rs` |

UI changes to `tui.rs` / `editor.rs` come with render or key tests (AGENTS.md).

## Shared: `zen-proto` (`crates/zen-proto`, 48 lines)

`text_of` (message content as text, Pi's format), `head`, `tail`. Used by `zend` (`compact`,
`flow`, `score`), `zen-engine` (`turn`) and `zen` (`main`, `tui`). Changing how content is read
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
| `sessions` | 0001; `effort` 0003; `state`, `parent`, `kind` 0011; `workspace` 0013; model ids rewritten 0002 | One per session (one job). `kind = 'verifier'` for child sessions; `workspace` overrides the kernel's | `api` (create/update), `turns` (title, child sessions), `flow` (state, routed model), `tape::append` (`updated_at`) | `api`, `turns`, `flow`, `main.rs` (restart recovery), e2e |
| `tape_events` | 0001; `seq`, `parent`, `hash` + functions `zen_block_hash`, `zen_rechain` 0007; index `(session_id, kind, seq)` 0014 | The tape. Kinds: `message`, `base`, `envelope`, `context`, `compaction`, `engine_session`, `state`, `brief`, `approval`, `questions`, `ruling`, `submission`, `verdict`, `verification`, `report` | `tape::append` only (callers: `dispatch`, `turns`, `compile`, `compact`, `flow`) | `tape::load*`, `compact`, `flow`, e2e (`tape_is_sound`) |
| `model_calls` | 0001; `turn_id`, `duration_ms` 0004; index 0012; partial index on untraced calls (`turn_id IS NULL`) 0014 | One row per assistant message: tokens, cache, cost | `dispatch` (`turn.message`) | `turns`, `measure`, `session_cost` |
| `tool_calls` | 0001; `turn_id` 0004; index 0012 | One row per tool call | `dispatch` (`tool.call`) | `turns` |
| `turns` | 0004; `envelope`, `context`, `context_tokens`, `cache_break` 0009 | One per turn: harness, worker, engine, model, effort, outcome, totals, what was sent | `turns` (`begin_turn` insert, `record_turn` update) | `api`, `flow`, `measure`, `score`, `turns`, e2e, evals |
| `session_decisions` | 0005; `source` 0011 | The owner's verdict (`accept/more/reshape/drop`; ground truth), or the model's on auto-close (`source`) | `api::decide`, `flow::report` | e2e (scoring compares against it later) |
| `session_scores` | 0006; index `(turn_id, trigger)` 0014 | System One answers about a session | `score` | `score` |
| `envelopes` | 0008 | System prompt + tools, stored once per distinct pair, keyed by hash | `compile::envelope` | `compile` |
| `compactions` | 0010 | How each summary was made; applied ones have `applied_seq` | `compact` | `compact`, e2e |
| `decisions` | 0011 | System One decisions (route, work, claim, model, `decide` tool) and what actually happened | `flow` (`log_decision`, `resolve_shadow`) | `flow` |
| `policies` | 0011 | Versioned routing policy (model and effort per kind of work); latest row in force | **nothing in the repo writes it** | `flow::route_for` (on approval) |

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
| `ZEN_BRIEFS` | `opt-in` (`always`, `off`/`0`) | `flow.rs` | Briefed workflow mode |
| `ZEN_AUTO_APPROVE` | `quick,bounded` | `flow.rs` | Routes approved without the owner (`all` allowed) |
| `ZEN_AUTO_CLOSE` | `quick,bounded` | `flow.rs` | Routes the model may close itself |
| `ZEN_FRESH_CONTEXT` | `architectural` | `flow.rs` | Routes whose work starts in a fresh context |
| `ZEN_VERIFY_ROUNDS` | `2` | `flow.rs` | Failed verifications sent back before reporting |
| `ZEN_VERIFY_SAMPLE` | `0.2` | `flow.rs` | Share of command-only work also checked by a model verifier |
| `ZEN_DECIDE_TOOL` | on | `flow.rs` | `0` hides the `decide` tool |
| `ZEN_S1_MODEL` | unset (off) | `score.rs` | System One classifier: scoring, shadow decisions, `decide` |
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
| `ZEN_DEV_PORT` / `ZEN_DEV_DB` | `18100` / `zen_dev` | `dev.sh` | Dev kernel port and database |
| `ZEN_E2E_PORT` / `ZEN_E2E_KEEP` / `ZEN_E2E_NO_BUILD` | `18377` / – / – | `e2e.sh` | e2e kernel port; keep DB and files; skip the build (CI) |
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
| `outputs/` | `tools.rs` | the model (`read`) | Full output of cut tool results |
| `engine/claude/`, `engine/complete/` | `claude.rs` | Claude Code | Fixed directories for engine sessions and completions |
| `AGENTS.md` | owner | `context.rs` | Global instructions for every session (optional) |
| `evals/<run>/` | `eval.sh` | `eval-report.sh` | Eval results |

Claude Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`.

---

## Scripts, install, deploy, CI

| Path | Does | Touches |
|---|---|---|
| `install.sh` | Fresh-VM install (safe to re-run): apt packages (git, curl, jq, bubblewrap, docker), Node 22 in `~/.local/node`, Claude Code and Codex CLIs, `npm ci` for Pi if enabled, binaries (download or build), `~/.zenbot/{bin,env,token,version}`, systemd units, git hooks path | system, `~/.zenbot`, `/etc/systemd/system` |
| `scripts/upgrade.sh` | Build (or fetch) → `cargo test` (local builds) → ping workers → smoke turn per worker on a second kernel (`:18199`, throwaway copy of the live DB, so migrations are tried there) → schedule `apply-upgrade.sh` via `systemd-run`. `--check` stops before scheduling | `target/`, temp DB `zen_smoke_*` |
| `scripts/apply-upgrade.sh` | Detached: wait for `"busy":0` (up to 30 min, then goes ahead), back up DB if migrations are pending, swap binaries (keeps `.prev`), write `version`, restart, health check, roll back if unhealthy | `~/.zenbot/{bin,version,upgrade.log,backups}`, service |
| `scripts/self-update.sh` | `git pull` main then `upgrade.sh`; refuses a dirty checkout or another branch. Used by `zen upgrade`, `/upgrade`, `POST /api/upgrade` | checkout |
| `scripts/fetch-release.sh` | Download CI's binaries for HEAD into `target/release` (checksum verified); fails (changing nothing) on local changes under `crates/`, `Cargo.*`, non-x86_64-Linux, or no build. `--check REF` only checks | `target/release` |
| `scripts/update-engines.sh` | Daily (timer): update Claude Code / Codex CLIs, test with a real `complete` through `zen-engine`, roll back on failure; Pi only reported. `--check` reports only | CLI installs, `upgrade.log`, `engines.json` |
| `scripts/db.sh` | DB helpers run inside the Postgres container: `pending`, `backup`, copy/drop/restore helpers | live DB, `~/.zenbot/backups` |
| `scripts/lib.sh` | `zen_env`, `pi_enabled`, `wait_healthy`, `ensure_rust`, `new_token` | — |
| `scripts/dev.sh` | Dev kernel in the foreground on `:18100` with database `zen_dev`, using `~/.zenbot/env` settings | `zen_dev` DB |
| `scripts/e2e.sh` | End-to-end scenarios (below); scripts in `scripts/e2e/*.json`, plus `scripts/e2e/slow_worker.py` (a Python worker serving `slow/summarizer`, a deliberately slow `complete`) | temp DB and workspace |
| `scripts/eval.sh`, `scripts/eval-report.sh` | Harness eval: this checkout vs installed (or `--base REF`), same model; report for the owner, never a gate | `zen_eval_*` DBs, `~/.zenbot/evals/` |
| `scripts/git-hooks/prepare-commit-msg` | Adds `Zen-Session` / `Co-Authored-By` trailers when `ZEN_SESSION_ID` is set | commit messages |
| `deploy/compose.yaml` | Postgres + pgvector only | Docker volume `zen-pg` |
| `deploy/zenbot.service` | Runs `~/.zenbot/bin/zend` with `EnvironmentFile=~/.zenbot/env`; `ExecStartPre` brings Postgres up; `Restart=always` | — |
| `deploy/zen-engines.service` / `.timer` | `update-engines.sh` daily at 04:00 UTC (+ up to 1 h random delay) | — |
| `.github/workflows/ci.yml` | On PRs and pushes to main: build, `cargo test`, clippy `-D warnings`, `scripts/e2e.sh`. On main, if green: build `dist` profile, publish `zenbot-x86_64-linux-<sha12>.tar.gz` to the rolling `edge` release (20 newest kept) | GitHub releases |

---

## Tests

- **Unit tests** live in `#[cfg(test)] mod tests` at the bottom of each file: `zend` (`tools` 10,
  `flow` 4, `compact` 3, `compile` 3, `score` 3, `context` 2, `measure` 2, `secrets` 2, `api` 1,
  `git` 1), `zen` (`tui` 16 render/key tests, `editor` 5, `client` 1, `main` 1), `zen-engine`
  (`claude` 3, `codex` 2, `turn` 2), `zen-proto` 1. Run `cargo test --release`.
- **End to end:** `scripts/e2e.sh [filter]` builds, then runs each scenario on a fresh kernel (port
  18377, `ZEN_FAUX=1`, `ZEN_WORKERS=engine`) with its own git workspace, all on one throwaway
  database (`zen_e2e_<pid>`), checking the database. At the end it checks every tape is numbered
  without gaps and its hash chain recomputes. Scenarios (10): `open-loop`, `restart-recovery`, `workflow-pass`,
  `workflow-approval-and-rounds`, `opt-in`, `verifier`, `summaries`, `secrets`, `slow-summary` (a
  summary at the hard limit slower than the watchdog: the turn waits, then runs), `stale-turn` (a
  turn the kernel ended keeps running in the worker; its late answer must not reach the next turn).
  Faux scripts in `scripts/e2e/*.json`; `slow-summary` adds the `slow` worker from
  `scripts/e2e/slow_worker.py` via `ZEN_WORKER_SLOW_CMD`. Add a scenario when kernel behavior changes.
- **Evals:** `evals/tasks/<name>/{task.json,files/}` (13 tasks, including the runner self-test
  `smoke`), run by `scripts/eval.sh`; see `evals/README.md`. Harness changes (system prompt,
  history, tools, workers, model or effort handling) get an eval before commit; the report goes to
  the owner, who decides.
- **Upgrade smoke test:** one scripted turn per worker inside `scripts/upgrade.sh`.

---

## Notes and known gaps

- **`policies` is never written.** `flow::route_for` reads the latest row on approval, but no code,
  migration or script inserts one, so routing by policy does nothing unless a row is added by hand.
- **`flow.rs` header is out of date on the start state.** It says a session "starts in `framing`";
  with the default `ZEN_BRIEFS=opt-in` it starts `open`, and only `always` starts in `framing`.
- **The briefed workflow is slated to change.** The target design agreed on 2026-10-06
  (DESIGN.md "Target design", DECISIONS.md D-025 to D-031) replaces the kernel-enforced workflow of
  `docs/brief.md`; ROADMAP.md Phase 1 removes the briefed-work gates. `flow.rs` still implements
  the old one. Check those before extending `flow.rs`.
- **Criteria commands run unsandboxed.** `flow::run_check` now runs a brief's `run` commands on the
  bash tool's core (`tools::run_shell`: own process group, 600 s timeout, output masked), but with
  `read_only = false`, so outside bubblewrap and able to write, even though the verifier's own shell
  is read-only.
- **Workers start through a login shell** (`bash -lc` in `mind.rs`), so the service user's
  profile can change their environment.
- Instruction files attached to tool results (`dispatch.rs`) are appended after masking, so they
  are not masked.
- **`codex.rs` header says threads are ephemeral**, but the code keeps them across turns unless
  `ZEN_CODEX_RESUME=0`.
- The protocol as built is in `docs/worker-protocol.md`; any sketch of it in `SPEC.md` is a target,
  not the code.

---

## Deeper docs

| Doc | What's there |
|---|---|
| `AGENTS.md` | How to change zenbot: build, test, upgrade, conventions, rules. Read it fully first |
| `CONTEXT.md` | What zenbot is for, how success is measured, constraints. Read first |
| `DESIGN.md` | How the system works today, then the target design agreed on 2026-10-06 |
| `DEVELOPMENT.md` | Local loop: build, test, verify and ship a change |
| `ROADMAP.md` | Phases in order (Phase 0 active; Phase 1 removes the briefed-work gates) |
| `PROGRESS.md` | Append-only log of what shipped and what was learned |
| `DECISIONS.md` | Why things are the way they are (D-025 to D-031: the target design) |
| `SPEC.md` | Long-term target modules (section numbers kept, e.g. 5.18 Security) |
| `docs/context.md` | How each turn's context is built, stored, cached, summarized and measured; the tape |
| `docs/brief.md` | The briefed workflow implemented in `flow.rs` |
| `docs/worker-protocol.md` | Kernel ⇄ worker JSON-RPC: methods, messages, engine sessions, guarantees |
| `docs/client-protocol.md` | HTTP API and WebSocket events for clients |
| `evals/README.md` | Eval tasks and the report |
| `README.md`, `INSTALL.md` | Using and installing zenbot |
