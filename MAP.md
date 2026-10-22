# zenbot — Codebase Map

Find what you'll touch, what it depends on and what depends on it, then read the code. Each
module's design notes are its header comment; how the system works is [DESIGN.md](DESIGN.md). When
this map and the code disagree, the code wins: fix the map.

```
 owner ── zen (crates/zen, terminal) ─┐   zen-matrix (crates/zen-matrix, Matrix rooms, own service)
                                      ▼   HTTP /api/* + WebSocket /api/sessions/{id}/ws, Bearer token
 zend (crates/zend, kernel) ── sqlx ──▶ private SQLite + sqlite-vec + FTS5 (<zen home>/zen.sqlite3)
   │ owns all state, runs every tool call, keeps the tape
   │ web.rs ──▶ SearXNG / Brave / Tavily, public pages     mcp.rs ──▶ the owner's MCP servers
   │ s1/, search.rs ──▶ OpenRouter /systemone, /embeddings (when configured)
   │ JSON-RPC over stdio (docs/worker-protocol.md)
   └──▶ zen-engine (crates/zen-engine): claude (`claude -p --tools ""` + mcp-bridge),
        codex (`codex app-server`, zenbot tools as dynamic tools), faux (scripted, ZEN_FAUX=1)
```

systemd runs `zend` (`deploy/zenbot.service`); `zend` starts workers as children and restarts them
(`workers.rs::supervise`). Never restart `zend` yourself: use `scripts/upgrade.sh`.

## Kernel `zend` (`crates/zend`)

One binary (axum, sqlx, tokio). Modules are declared in `src/main.rs` and share `App`. Paths below
are under `crates/zend/`.

| File | Role | Notable dependents |
|---|---|---|
| `src/main.rs` | Startup, `App`, router, background loops, `zen_home`, `valid_name`, event hubs (`subscribe`/`emit`) | everything |
| `src/config.rs` | Tunable `ZEN_*` settings read once into a typed `Config`, defaults in one place | most modules |
| `src/storage.rs` | SQLite connections (WAL, foreign keys, bundled sqlite-vec), `begin` immediate transactions, migrations | `tape`, `sources`, `gate`, `queue`, `jobs`, `brain` |
| `src/api.rs` | HTTP handlers, token auth middleware, session WebSocket | router |
| `src/turns.rs` | Turn lifecycle: start, compile, record, finish, abort, watchdog, child sessions (`run_child`), queued prompts | `api`, `dispatch`, `agent`, `delegate`, `jobs` |
| `src/dispatch.rs` | Every worker message: tool calls (refusals, gate, taint, AGENTS.md attachments), deltas, usage, `turn.end` | `main` |
| `src/agent.rs` | Agent tools beyond files and shell; per-kind allow-lists; protected-file refusals; `verify`, `ask`, `judge` | `dispatch`, `turns` |
| `src/tools.rs` | `bash`, `read`, `write`, `edit`; `run_shell`; `bwrap` sandboxes; output cut and saved | `dispatch`, `agent`, `jobs`, `mcp`, `web` |
| `src/fsview.rs` | What a shell may see: masks by content rating, rates shell results by shown lines | `dispatch`, `tools`, `agent` |
| `src/media.rs` | Bounded image and PDF-page inspection for `read`, contained conversion | `tools`, `mcp` |
| `src/output.rs` | Preview limits, saved-output files under `outputs/`, retention | `tools`, `dispatch`, `mcp`, `jobs` |
| `src/compile.rs` | What a turn sends, in cache order: envelope, summary, history, prompt; system prompt from prompt files | `turns`, `measure`, `jobs` |
| `src/context.rs` | AGENTS.md/CLAUDE.md attachment: trusted repos as instructions, others as untrusted | `compile`, `dispatch` |
| `src/compact.rs` | Summaries of older turns; the `history` tool | `turns`, `dispatch` |
| `src/measure.rs` | Per-turn record of what was sent and why the prompt cache broke | `turns` |
| `src/tape.rs` | The tape: hashed append-only blocks, load, repair, JSONL mirror (`mirror_loop`) | nearly everything |
| `src/taint.rs` | Untrusted envelope, session risk/taint, `hand_over_rating` (the one read path for stored content) | most tool modules |
| `src/sources.rs`, `src/sources/` | Source ratings: owner allowlist, System One judgments, proposals, re-rating; byte ratings | `web`, `dispatch`, `gate`, `taint`, `memory`, `brain` |
| `src/session_state.rs` | Per-tool-call permission snapshot, invalidated by risk/lock/trust changes | `dispatch`, `gate`, `taint` |
| `src/task.rs` | The owner's goal as the gate's alignment question sees it (owner tape only) | `gate` |
| `src/gate.rs` | Action gate: classify effect, run/hold/lock, shell mode, egress decisions (D-067) | `dispatch`, `agent`, `proxy`, `queue` |
| `src/queue.rs` | Owner queue: held actions and source proposals, exact single-use grants | `api` routes |
| `src/proxy.rs` | Per-session egress proxy socket; `zend bridge` inside the sandbox | `tools`, `dispatch`, `agent`, `jobs` |
| `src/trust.rs` | The owner's `trust.toml` (url/api/mcp/repo levels), built-in registries | `sources`, `context`, `mcp`, `gate`, `proxy` |
| `src/secrets.rs` | Masks secrets in tool output; `is_secret_var`; `secret_files` hidden from shells | `tools`, `mind`, `agent`, `web`, `mcp` |
| `src/memory.rs` | Short-term memory (`global/MEMORY.md`), `remember`, capture, sleep trigger, prompt-file backups | `agent`, `compile`, `api`, `turns`, `jobs` |
| `src/brain/` | Record stores (memory, knowledge, entities) as Markdown files + index; the sleep; legacy wiki migration (D-069, D-070) | `agent`, `api`, `search`, `memory` |
| `src/wiki.rs` | Read-only compatibility for pre-D-069 flat wiki pages | `brain`, `search`, `sources` |
| `src/search.rs` | Index of sessions, memories, wiki and records; text + vector + exact search; the `search` tool | `agent`, `main` |
| `src/skills.rs` | Skills: frontmatter, validation, index, `find_skills`/`load_skill` (drafts too) | `agent`, `compile`, `workshop` |
| `src/workshop.rs` | Skill drafts, review by fingerprint, usage, archival; retired made tools (refusal only) | `agent`, `api`, `jobs`, `mcp` |
| `src/defaults.rs` | Writes missing default prompt files and skills from `crates/zend/defaults/` | `main` |
| `src/layout.rs` | Where files live under the zen home; one-time move from the flat layout (D-040) | `compile`, `memory`, `skills`, `brain` |
| `src/web.rs` | `web_search`, `web_fetch` (public addresses only, HTML to markdown, PDFs saved) | `agent` |
| `src/mcp.rs`, `src/mcp/` | MCP client (stdio, streamable HTTP), `find_tools`/`load_tool`/`call_tool`; OAuth (`auth.rs`, `login.rs`, D-072) | `agent`, `api` |
| `src/mcp_interaction.rs` | MCP forms (elicitation) answered only by the owner | `mcp`, `api` |
| `src/delegate.rs` | `delegate` tool, subagents, routing policy, nightly `routes` tuning | `agent`, `api`, `jobs`, `route` |
| `src/route.rs` | Learning router (D-074): task kind via System One, picks a session's model | `turns`, `jobs`, `delegate` |
| `src/jobs.rs` | Scheduled jobs (D-046): schedules, store, gate, system jobs, script/agent runs, report delivery, `schedule` tool | `main`, `agent`, `api` |
| `src/assist.rs` | Session names and next-prompt suggestions by the assist model (D-047) | `turns`, `api` |
| `src/prices.rs` | API-equivalent prices for engines without cost reports (D-075) | `turns`, `main` |
| `src/workers.rs` | Worker configs, supervision, model routing, `complete` | `main`, `api`, `turns`, `compact`, `assist`, `delegate` |
| `src/mind.rs` | JSON-RPC client for one worker process (secret env removed) | `workers` |
| `src/s1/` | System One (D-066): TOML decision files, checks, model ladder, ledger, learning, API | every judging module |
| `src/update.rs` | Self-update: compare with `origin/main`, run `scripts/self-update.sh` | `api`, `main` |
| `src/git.rs` | The one place the kernel runs git (time and output limits) | `update`, `agent`, `workshop` |
| `steps/verify.md` | The verifier's system prompt | `agent` |
| `defaults/` | Default `SOUL.md`, `IDENTITY.md`, `AGENTS.default.md`, `USER.md`, skills `work/{brief,close,search,verify}` | `defaults` |
| `s1/*.toml` | Built-in System One decision files | `s1` |
| `sqlite-migrations/` | SQLite schema (Database, below) | `storage` |
| `migrations/` | Historical PostgreSQL migrations, immutable; source for the storage migration | `scripts/storage.py` |
| `build.rs`, `vendor/sqlite-vec/` | Builds the pinned sqlite-vec statically into `zend` | — |
| `public-suffix-list.dat` | Domain boundaries for source ratings | `sources` |

### Routes

Everything under `/api` needs `Authorization: Bearer <token>`, never in the URL.

| Method | Path | Purpose |
|---|---|---|
| GET | `/health` | No auth: db, workers, busy count, version |
| GET | `/api/version` | Running commit vs `origin/main` |
| GET/POST | `/api/upgrade` | Upgrade status / start `scripts/self-update.sh` |
| GET | `/api/models` | Worker models, default, System One model |
| GET/POST | `/api/sessions` | List (`?archived=`) / create sessions |
| GET/PATCH | `/api/sessions/{id}` | Messages (`?last=`, `?after=`) / title, model, effort, archived |
| GET | `/api/sessions/{id}/ws` | Session WebSocket (docs/client-protocol.md) |
| POST | `/api/sessions/{id}/decision` | Owner verdict: accept, more, reshape, drop |
| GET | `/api/sessions/{id}/taint` | What marked the session |
| POST | `/api/sessions/{id}/trust` | Owner lifts the taint (D-054) |
| POST | `/api/sessions/{id}/deny` | Owner stop: contaminate session and descendants (D-062) |
| GET/POST | `/api/sessions/{id}/allow` | Held actions / owner's exact grant |
| GET | `/api/board` | Sessions board: sessions with children, busy, waiting |
| GET | `/api/queue` | Pending held actions and source proposals |
| POST | `/api/queue/{kind}/{id}` | Approve, reject or deny one |
| GET/POST | `/api/sources` | Source allowlist / change it |
| POST | `/api/sources/rerate` | Re-rate stored web bytes |
| GET/POST | `/api/trust` | Trust list / change it |
| GET | `/api/s1` | System One stats per decision |
| GET | `/api/s1/check` | Decision files in force and their problems |
| GET | `/api/s1/models` | Approved System One models |
| POST | `/api/s1/replay` | Replay a policy over recorded answers |
| POST | `/api/s1/compare` | Re-ask recorded states on other models |
| GET | `/api/s1/proposals` | Learner's pending proposals |
| POST | `/api/s1/proposals/{id}` | Approve or reject one |
| GET | `/api/memory` | Memories by tier, last sleep |
| POST | `/api/memory/sleep` | Run the sleep now |
| POST | `/api/brain` | Owner record actions (get, list, put, lint, index, migrate_legacy) |
| GET | `/api/suggestions` | Suggested-prompt outcomes |
| GET/POST | `/api/jobs` | List / create jobs |
| PATCH/DELETE | `/api/jobs/{name}` | Change, pause, resume / remove a job |
| POST | `/api/jobs/{name}/run` | Run a job now |
| GET | `/api/jobs/runs` | Recent runs (`?job=`, `?limit=`) |
| GET | `/api/mcp` | MCP config, cached tool counts, problems |
| POST | `/api/mcp/login` | Start an MCP OAuth sign-in |
| POST | `/api/mcp/login/{flow}` | Finish it with the callback address |
| GET | `/api/mcp/pending` | MCP forms waiting for the owner |
| POST | `/api/mcp/pending/{id}` | Answer one form |
| GET/POST | `/api/policy` | Routing policy and evidence / new version |
| POST | `/api/policy/undo` | Restore the previous version |
| GET | `/api/skills` | Skills, drafts, usage, fingerprints |
| POST | `/api/skills/review` | Accept or reject a draft by fingerprint |
| POST | `/api/tools/{name}/review` | Always refused: made tools are retired |

### Tools the model gets

A fixed list per session, in this order (part of the cached prefix): `bash`, `read`, `write`,
`edit` (`tools.rs`), then `history`, `search`, `ask`, `remember`, `brain`, `web_search`,
`web_fetch`, `find_skills`, `load_skill`, `save_skill`, `find_tools`, `load_tool`, `call_tool`,
`verify`, `delegate`, `schedule` (`agent.rs`, handing off to their modules), then `judge` when
System One may see private content. Subagent and job sessions get no `ask`, `delegate` or
`schedule`; a verifier gets only `bash`, `read` and `submit_verdict`. MCP tools never join the
list: they go through `find_tools`/`load_tool`/`call_tool`.

## Worker `zen-engine` (`crates/zen-engine`)

Protocol: `docs/worker-protocol.md`, typed in `crates/zen-proto/src/worker.rs`. A protocol change
updates the doc, the types and every worker.

| File | Role |
|---|---|
| `src/main.rs` | Serves the protocol on stdio; subcommands `mcp-bridge`, `codex-features`; routes turns by model prefix |
| `src/engine.rs` | `Engine` trait and registry (claude, codex, faux) |
| `src/turn.rs` | Per-turn context, tool socket to the kernel, `engine_dir` under `<zen home>/engine/` |
| `src/rpc.rs` | JSON-RPC over stdio |
| `src/bridge.rs` | The stdio MCP server Claude Code launches; forwards to the engine's socket |
| `src/claude.rs` | Claude Code engine: `claude -p`, own tools off, checked at `system/init` |
| `src/codex.rs` | Codex engine: `codex app-server`, unknown features/items refused |
| `src/failover.rs` | One cross-provider continuation on a subscription quota error (D-044) |
| `src/faux.rs` | Scripted models `faux/smoke`, `faux/backup` for tests |

## CLI `zen` (`crates/zen`)

| File | Role |
|---|---|
| `src/main.rs`, `src/cli.rs` | Entry and clap commands (`ask`, `chat`, `sessions`, `jobs`, `skills`, `policy`, `status`, …) |
| `src/turn.rs` | `zen ask` / line chat: stream one turn over the WebSocket |
| `src/setup.rs` | Reads `env`, `engines.json`; `zen login`; status lines |
| `src/print.rs`, `src/md.rs`, `src/screen.rs` | Script output; markdown rendering and sanitizing; full-screen frames |
| `src/client.rs` | `zen-client` re-exported plus zen-only helpers |
| `src/editor.rs` | Multi-line input editor with history |
| `src/tui/` | Interactive app (board, transcript, panel, pickers, inline mode); render/key tests use `tui/test_util.rs` |

## Shared crates

- **`crates/zen-proto`**: `lib.rs` (message text helpers), `client.rs` (client protocol events,
  `Event::ends_turn`), `worker.rs` (worker protocol messages). Used by all four binaries: a change
  here changes them all, and the matching doc.
- **`crates/zen-client`**: `lib.rs` (`zen_home`, `parse_env`, `Client`, `Ws`), `owner.rs` (the
  owner's chat commands `jobs`, `skills`, `tools`, `trust`). Used by `zen` and `zen-matrix`.
- **`crates/zen-matrix`** (own Cargo workspace, own CI job, D-049): `main.rs` (Matrix client,
  rooms, relay loop), `trust.rs` (who and where it answers), `relay.rs` (events → posts),
  `kernel.rs` (API calls), `state.rs` (`matrix.env`, `state.json`); `examples/owner.rs` drives
  `scripts/matrix-e2e.sh`. Installed by `scripts/matrix.sh`, rebuilt by `scripts/upgrade.sh`;
  details in `docs/matrix.md`.

## Database

Private SQLite at `<zen home>/zen.sqlite3` (`storage.rs`): WAL, foreign keys, bundled sqlite-vec
and FTS5. Schema: `crates/zend/sqlite-migrations/`, expand-only (AGENTS.md); `zend` ignores
migrations it doesn't know so a rolled-back build starts on a newer schema. Views
`zen_contaminated_sessions`, `zen_contaminated_memories`; triggers keep provenance, the search
queue and tape exports.

| Table(s) | Holds | Written by |
|---|---|---|
| `sessions` | One per session; `kind` NULL (owner), `subagent`, `verifier`, `job`; risk, taint, lock, model | `api`, `turns`, `jobs`, `taint`, `gate` |
| `tape_events` | The tape: hashed, sequenced blocks per session | `tape` |
| `tape_exports` | Sessions whose JSONL mirror needs rewriting | trigger; read by `tape::mirror_loop` |
| `pending_prompts` | Queued owner prompts and steering | `turns` |
| `turns`, `model_calls`, `envelopes` | Per-turn record, per-message tokens and cost, stored prefixes | `turns`, `dispatch`, `compile` |
| `tool_calls` | Every tool call, arguments masked and cut | `dispatch` |
| `compactions` | How each summary was made | `compact` |
| `item_ratings`, `contamination`, `session_inputs` | Byte ratings, per-cause contamination, reader/writer edges | `sources`, `taint`, `jobs`, triggers |
| `sources`, `source_audit`, `source_vouches` | Owner allowlist, its audit, owner-page adjustments | `sources` |
| `source_ratings`, `source_proposals`, `source_rerates` | System One source judgments, owner proposals, re-rate marker | `sources` |
| `action_gates`, `egress` | Held actions, locks, egress holds; hosts each shell reached | `gate`, `queue` |
| `mcp_interactions` | MCP forms waiting for or answered by the owner | `mcp_interaction` |
| `decisions`, `s1_states`, `s1_outcomes`, `s1_calibration`, `s1_proposals` | System One ledger, masked states, outcomes, calibration, proposals | `s1` |
| `session_decisions` | The owner's verdicts (ground truth) | `api` |
| `session_scores` | Legacy session scores; nothing reads or writes them | — |
| `memories`, `brain_memory_inputs` | Short-term memories; every session that wrote one | `memory`, trigger |
| `brain_records`, `brain_aliases`, `brain_links` | Record index rebuilt from the files | `brain` |
| `brain_skill_uses` | Skill version each session loaded, and the verdict | `skills`, `workshop` |
| `sleep_runs` | One row per sleep | `memory` |
| `search_docs`, `search_state`, `search_queue`, `searches` | Search documents with embeddings, watermark, memory queue, search log | `search`, triggers |
| `jobs`, `job_runs` | Scheduled jobs and their runs | `jobs` |
| `policies` | Versioned routing policy | `delegate`, `api` |
| `prompt_suggestions` | Suggested next prompts and outcomes | `assist` |
| `draft_taint` | Draft skills written by a tainted session | `workshop` |
| `made_tools` | Legacy made-tool approvals; authorize nothing | — |

## Settings

The service reads `<zen home>/env` (systemd `EnvironmentFile`). Kernel tunables are read once in
`config.rs`; boot settings, paths and worker settings where used. Workers inherit the kernel's
environment minus secret-looking variables (`secrets::is_secret_var`), engine sign-ins kept; the
agent's shells get none of them.

### Kernel

| Name | Default | What |
|---|---|---|
| ZEN_TOKEN | required | API token (masked in output) |
| `DATABASE_URL` | `sqlite://<zen home>/zen.sqlite3` | Database; must be SQLite |
| ZEN_PORT / ZEN_BIND | `8100` / `127.0.0.1` | Listen port and address (D-041) |
| ZEN_HOME | `~/.zenbot` | Zen home (also `zen`, `zen-engine`, `zen-matrix`) |
| ZEN_WORKSPACE | `$HOME` | Default working directory |
| ZEN_REPO | `$HOME/zenbot` | zenbot's checkout: updater, trusted AGENTS.md |
| ZEN_TRUSTED_REPOS | none | More repos whose AGENTS.md are instructions |
| ZEN_DEFAULT_MODEL | `claude/claude-opus-5-5` | Model for new sessions |
| ZEN_HARNESS | installed commit | Build id recorded per turn |
| ZEN_WORKERS | `engine` | Workers to start; others need `ZEN_WORKER_<NAME>_CMD` |
| ZEN_ENGINE_CMD | `zen-engine` beside `zend` | Command for `engine` |
| ZEN_MODELS | all | Visible model allowlist and order |
| ZEN_SOUL_CHARS / ZEN_IDENTITY_CHARS / ZEN_AGENTS_CHARS / ZEN_USER_CHARS | 4000 / 4000 / 12000 / 3000 | Prompt-file caps |
| ZEN_SKILLS_DIR / ZEN_TOOLS_DIR / ZEN_WIKI_DIR | under `global/` | Skills, retired tools, wiki |
| ZEN_SKILL_INDEX_CHARS | 2500 | Skills index size before domains only |
| ZEN_MEMORY_CHARS | 4000 | Short-term memory size before a sleep |
| ZEN_INDEX_SECS | 20 | Search indexer pause |
| ZEN_EMBED / ZEN_EMBED_MODEL / ZEN_EMBED_URL | on / `openai/text-embedding-3-small` / OpenRouter | Embeddings (1536 dims) |
| ZEN_S1_MODEL | unset (off) | System One model without `s1/models.toml` |
| ZEN_S1_URL | OpenRouter `/systemone` | System One endpoint |
| ZEN_S1_PRIVATE | on | `0`: no private content to OpenRouter |
| ZEN_S1_CONCURRENCY | 8 | System One calls in flight |
| ZEN_JUDGE_TOOL | on | `0` hides `judge` |
| ZEN_JOB_BAR | 0.9 | Deprecated; overrides built-in `job_approval` costs when set |
| ZEN_SEARCH_PROVIDER | by key: brave, tavily, else searxng | Web search provider |
| ZEN_SEARXNG_URL | unset | External SearXNG |
| ZEN_SEARCH_RERANK | on | System One reranks web results |
| ZEN_MCP_CONFIG | `<zen home>/mcp.json` | MCP servers file |
| ZEN_CONTEXT_TOKENS | 200000 | Context budget |
| ZEN_COMPACT_SOFT / ZEN_COMPACT_HARD / ZEN_COMPACT_KEEP | 0.7 / 0.9 / 0.3 | Summary thresholds |
| ZEN_COMPACT_IDLE_SECS | 300 | Pause before applying a summary |
| ZEN_SUMMARY_MODEL | `claude/claude-sonnet-5-5` | Summarizer |
| ZEN_ASSIST_MODEL | `claude/claude-haiku-5-5` | Names and suggestions |
| ZEN_SUGGEST | on | Next-prompt suggestions |
| ZEN_CACHE_TTL | `claude=3600` | Prompt cache life per engine |
| ZEN_TURN_IDLE_SECS / ZEN_TURN_ABORT_GRACE_SECS | 600 / 30 | Watchdog |
| ZEN_ENGINE_CONCURRENCY | 3 | Background engine slots (D-075) |
| ZEN_EXPLORE / ZEN_POLICY_MIN_JUDGED | 0.1 / 20 | Routing exploration, evidence before a switch |
| ZEN_JOBS / ZEN_JOBS_TICK / ZEN_TZ | on / 60 / `America/Sao_Paulo` | Scheduler, its tick, default job zone |
| ZEN_UPDATE_CHECK_SECS | 3600 | Update check interval (`0` off) |
| ZEN_BRIDGE_BIN | running `zend` | Proxy bridge binary (tests) |
| ZEN_EGRESS_LOOPBACK_FOR_TESTS | unset | Tests: proxy may reach 127.0.0.1 |
| ZEN_LEGACY_TOOL_AUTHORING | unset | Ignored; e2e checks it can't revive made tools |
| ZEN_SESSION_ID / ZEN_MODEL | set per `bash` | Read by `scripts/git-hooks/prepare-commit-msg` |

Keys (secret, masked, kept from shells): `OPENROUTER_API_KEY`, `BRAVE_API_KEY`, `TAVILY_API_KEY`.

### Workers

| Name | Default | What |
|---|---|---|
| ZEN_FAUX / ZEN_FAUX_SCRIPT | off / built-in | Scripted `faux/*` models and their steps |
| ZEN_CLAUDE_RESUME / ZEN_CODEX_RESUME | on | Resume engine sessions/threads |
| ZEN_CODEX_INJECT | on | History as native Codex items |
| ZEN_QUOTA_FAILOVER | on | Quota failover (D-044) |
| ZEN_FAILOVER_CLAUDE_TO_CODEX / ZEN_FAILOVER_CODEX_TO_CLAUDE | built-in | Failover target models |

`CLAUDE_CONFIG_DIR`, `CODEX_HOME`: the engines' own homes. `ANTHROPIC_API_KEY`,
`CLAUDE_CODE_OAUTH_TOKEN`, `OPENAI_API_KEY`, `CODEX_ACCESS_TOKEN`: when set, failover is refused.

### CLI, Matrix and scripts

| Name | Default | What |
|---|---|---|
| ZEN_URL / ZEN_TOKEN | `http://127.0.0.1:8100` / `<zen home>/token` | Kernel for `zen`, `zen-matrix` |
| ZEN_INLINE | off | `zen` inline mode |
| MATRIX_USER / MATRIX_OWNER | required | Bot account; the only account it answers |
| MATRIX_PASSWORD / MATRIX_RECOVERY_KEY | – | First sign-in; existing cross-signing |
| MATRIX_HOMESERVER | from user ID | Homeserver URL |
| ZEN_SMOKE_PORT | 18199 | `upgrade.sh` smoke kernel |
| ZEN_DEV_PORT / ZEN_DEV_DB / ZEN_DEV_HOME / ZEN_DEV_REAL | 18100 / `zen_dev` / `~/.zenbot-dev` / off | `dev.sh` |
| ZEN_DB_DIR | zen home | Where `db.sh` puts disposable databases |
| ZEN_E2E_PORT / ZEN_E2E_JOBS / ZEN_E2E_KEEP / ZEN_E2E_NO_BUILD / ZEN_E2E_BIN | 18377 / 4 / – / – / `target/release` | `e2e.sh` |
| ZEN_E2E_MCP_TOKEN | set by `e2e.sh` | `${VAR}` expansion test |
| ZEN_WORKER_SLOW_CMD / ZEN_WORKER_ENVDUMP_CMD / ZEN_SLOW_SECS | set by `e2e.sh` / – / 12 | Test workers |
| ZEN_MATRIX_E2E_PORT / ZEN_MATRIX_E2E_HS_PORT | 18197 / 16167 | `matrix-e2e.sh` |
| ZEN_EVAL_PORT / ZEN_EVAL_KEEP_BUILDS / ZEN_EVAL_JOBS / ZEN_EVAL_CACHE_DAYS | 18301 / 5 / 3 / 7 | `eval.sh` |
| ZEN_ENGINES / ZEN_ENGINES_FAIL / ZEN_ENGINES_CLAUDE_MODEL / ZEN_ENGINES_CODEX_MODEL | all / – / cheapest | `update-engines.sh` |
| ZEN_BUILD_FROM_SOURCE / ZEN_RELEASE_BASE | – / `edge` release | `fetch-release.sh` |
| ZEN_STORAGE_TOOL / ZEN_PG_COMPOSE / ZEN_MIGRATION_PG_URL | `scripts/storage.py` / `deploy/compose.yaml` / – | PostgreSQL → SQLite migration (URL never logged) |
| ZEN_UPGRADE_SERVICE_FILE | `/etc/systemd/system/zenbot.service` | `apply-upgrade.sh` (tests) |

## Zen home (`~/.zenbot/`)

Secret files (AGENTS.md lists them): never print them.

| Path | Written by | What |
|---|---|---|
| `env`, `env.prev` | `install.sh`, `apply-upgrade.sh` (`lib.sh::env_backup`) | Service environment; its previous copy |
| `token` | `lib.sh::new_token` | API token |
| `db-password` | older PostgreSQL installs | Legacy; still masked |
| `zen.sqlite3` (+ WAL/SHM) | `zend` | The database |
| `sessions/` | `tape::mirror_loop` | Private JSONL mirror of each session's tape |
| `version`, `upgrade.log`, `upgrades/` | `install.sh`, `upgrade.sh`, `apply-upgrade.sh` | Installed commit, upgrade log, staged builds |
| `bin/` | `install.sh`, `apply-upgrade.sh`, `scripts/matrix.sh` | `zend`, `zen`, `zen-engine` (+ `.prev`), `zen-matrix` |
| `engines.json`, `engines.lock` | `update-engines.sh` | Engine versions; update lock |
| `history` | `zen` | Prompt history |
| `backups/` | `db.sh`, `apply-upgrade.sh`, `memory::backup_file`, `brain::put` | Database dumps, prompt-file and record backups |
| `outputs/` | `output.rs` (tools, web, MCP) | Full masked tool output, web PDFs |
| `engine/` | `zen-engine` | Claude Code / Codex working dirs, completions, tool sockets |
| `proxy/` | `proxy.rs` | Per-session egress proxy sockets |
| `AGENTS.md`, `USER.md`, `agents/zenbot/{SOUL,IDENTITY}.md` | `defaults.rs`, then owner/agent | Prompt files |
| `global/MEMORY.md` | `memory::export`, owner | Short-term memory |
| `global/memory/`, `global/entities/`, `global/wiki/` | `brain::put`, owner | Long-term memory, entities, knowledge records |
| `global/skills/` | `defaults.rs`, `save_skill`, `skills` job | Skills; `_proposed/` drafts, `_archived/` |
| `global/tools/` | legacy | Retired made tools, inspection only |
| `s1/` | owner, `zen s1 approve` | System One overrides and `models.toml` |
| `trust.toml` | owner, `zen trust` | Trust list |
| `prices.toml` | owner | Price overrides |
| `mcp.json`, `mcp-auth/` | owner; `mcp/auth.rs`, `mcp/login.rs` | MCP servers; OAuth credentials |
| `matrix.env`, `matrix/`, `matrix.version` | owner; `zen-matrix`; `apply-upgrade.sh` | Matrix settings, session and state; built commit |
| `evals/` | `eval.sh` | Eval runs, cache, base builds |

## Scripts, deploy, CI

| Path | Does |
|---|---|
| `install.sh` | Fresh install: packages, CLIs, binaries, SQLite, systemd unit, token, git hooks |
| `scripts/upgrade.sh` | Build or fetch, check, smoke-test, stage, schedule `apply-upgrade.sh` |
| `scripts/apply-upgrade.sh` | Install the staged build when idle; backup, cutover, rollback |
| `scripts/self-update.sh` | Pull `main` then `upgrade.sh` (`zen upgrade`, `/api/upgrade`) |
| `scripts/fetch-release.sh` | Download CI's binaries for HEAD |
| `scripts/update-engines.sh` | Daily `engines` job: update and test the CLIs, roll back on failure |
| `scripts/db.sh`, `scripts/lib.sh` | Database helpers; shared shell helpers (`env_set`, `wait_healthy`, …) |
| `scripts/storage.py`, `scripts/migrate-storage.sh`, `scripts/sqlite-query.py` | PostgreSQL → SQLite export/restore/backup; migration prep; dev SQL |
| `scripts/test_*.py`, `scripts/test-sqlite.sh` | Storage and migration tests |
| `scripts/dev.sh` | Dev kernel on :18100 with its own home and database |
| `scripts/check.sh` | What CI checks: docs, S1 guard, build, tests, clippy, size budget, e2e |
| `scripts/check-docs.py` | Docs vs code (this map's lists, paths, budgets, PROGRESS entry) |
| `scripts/size-budget.sh` | Binary size budgets |
| `scripts/e2e.sh`, `scripts/e2e/` | End-to-end scenarios; faux scripts (`*.json`) and test servers/oracles (`*.py`) |
| `scripts/eval.sh`, `scripts/eval-report.sh`, `scripts/eval-native.sh`, `scripts/eval-paired.py` | Harness evals for the owner (`evals/README.md`, `evals/areas.txt`) |
| `scripts/matrix.sh`, `scripts/matrix-e2e.sh` | Install / test `zen-matrix` end to end (not in CI) |
| `scripts/git-hooks/prepare-commit-msg` | `Zen-Session` / `Co-Authored-By` trailers |
| `deploy/zenbot.service`, `deploy/zen-matrix.service` | systemd units |
| `deploy/compose.yaml`, `deploy/searxng/` | Optional PostgreSQL source for migration, local SearXNG |
| `.github/workflows/ci.yml` | `check`, `matrix`, `arm64` jobs; `edge` release on `main` |

## Tests

Unit tests sit at the bottom of each file. `scripts/e2e.sh [filter]` runs these scenarios (the
functions after each name; what each checks is the comment above its function):

| Scenario | Parts |
|---|---|
| `phase4`, `phase4-upgrade`, `phase3` | records and brain; upgrade from an old database; job authority and queued prompts |
| `turns`, `jobs` | abort, stale turns; scheduled, report-file, post-to and script jobs |
| `search-index`, `search`, `wiki` | indexer, recall, legacy wiki migration |
| `taint-on-data`, `trust`, `security`, `action-gate`, `file-trust` | taint, trust and origin attacks, egress proxy, gate and queue, kernel tools and secrets |
| `summaries`, `system-one`, `memory` | compaction, System One stub, memory and sleep |
| `reach`, `mcp-login` | MCP and web tools, OAuth sign-in |
| `routing`, `delegation`, `names-suggestions` | router, subagents and engine cap, assist |
| `basics`, `workshop`, `verifier`, `agent-tools` | prompt files, restart, layout; skills; verify; on-demand tools |
| `engine-boundary`, `failover-model` | engines' own tools off; quota failover |

Evals: `evals/tasks/`, run by `scripts/eval.sh`. Matrix: `scripts/matrix-e2e.sh`.
