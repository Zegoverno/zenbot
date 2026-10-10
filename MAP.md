# zenbot — Codebase Map

> **Purpose:** a blast-radius guide for coding agents. Before touching anything, find it here to see
> what it does, what it depends on and what depends on it. The map is not the territory: read the
> code before changing it, and if the two disagree, the code wins (then fix the map).
>
> Written from the code on 2026-10-06; audited against the code on 2026-10-10 at `6217f27` (every
> file, route, table, setting and script checked; line counts refreshed with `wc -l`). Where
> something couldn't be confirmed in the code it says "unverified". Line counts only show which
> files are big.

---

## Quick orientation

```
 owner
   │  terminal
   ▼
 zen (CLI, crates/zen)
   │  (also zen-matrix, crates/zen-matrix: the owner's Matrix rooms, a separate service, D-049)
   │  HTTP /api/* + WebSocket /api/sessions/{id}/ws   (Bearer token from ~/.zenbot/token)
   ▼
 zend (kernel, crates/zend) ───── sqlx ─────▶ Postgres + pgvector (Docker, deploy/compose.yaml)
   │  owns all state, runs every tool call, keeps the tape
   │  web.rs ──HTTP──▶ SearXNG (Docker, 127.0.0.1:8888) or Brave / Tavily; public web pages
   │  mcp.rs ──stdio / streamable HTTP──▶ the owner's MCP servers (~/.zenbot/mcp.json)
   │  search.rs / score.rs ──HTTPS──▶ OpenRouter /embeddings and /systemone (when configured)
   │  JSON-RPC 2.0 over stdio, one JSON object per line (docs/worker-protocol.md)
   ├──▶ zen-engine (crates/zen-engine)            default worker `engine`
   │      ├─ claude: `claude -p --tools ""` + MCP config pointing at
   │      │          `zen-engine mcp-bridge <socket>` ──Unix socket──▶ engine ──tool.call──▶ zend
   │      ├─ codex:  `codex app-server`, shell/apps/plugins off, zenbot tools as dynamic tools
   │      └─ faux:   scripted model `faux/smoke` (ZEN_FAUX=1), for tests
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
`use super::*`. ~12,000 lines of Rust plus migrations, default prompt files and skills, and the verifier's
prompt.

### Files

| File | Lines | Role | Main items | Depends on | Used by |
|---|---|---|---|---|---|
| `src/main.rs` | 290 | Startup and shared state. Reads env, writes missing default prompt files and skills (`defaults::install`), connects to Postgres, runs migrations (`set_ignore_missing(true)` so a rolled-back build starts on a newer schema), repairs the tape, spawns workers, starts background tasks (dispatch, watchdog, `score::idle_loop`, `search::index_loop`, `jobs::run_loop`, the updater), builds the router (listens on `ZEN_BIND`, localhost by default, D-041) | `App` (incl. `hubs`, and `background`: kernel work outside turns, counted as busy through `Background` guards), `zen_home` (`ZEN_HOME`, else `~/.zenbot`), `subscribe`/`unsubscribe`/`emit` (a session's event hub exists only while a client is connected; `emit` with no client sends nothing), `env_num`, `outputs_dir`, `load_messages`, `context_in` | every module | everything (via `App`) |
| `src/assist.rs` | 289 | Small model calls around the owner's turns (D-047): after an owner's turn ends cleanly (not in jobs or child sessions), one call to the assist model (`ZEN_ASSIST_MODEL`, Haiku 5.5) names the session while `title_source` isn't `owner` (first 3 turns) and suggests the next prompt (not after `ask`; `ZEN_SUGGEST=off` stops it); emits `title` and `suggestion`; `settle` records what the owner did with it when the next prompt arrives | `after_turn`, `settle`, `stats` (`GET /api/suggestions`), `PROMPT_VERSION`, `model` | `workers::complete`, `tape` | `turns.rs` (`finish_turn`), `api.rs` (`prompt`) |
| `src/api.rs` | 591 | HTTP handlers and the WebSocket (docs/client-protocol.md). Token auth middleware (header `Authorization: Bearer` only, compared in constant time; a token in the URL is refused). A WebSocket `prompt` starts its turn in a separate task, so an `abort` is read while the turn is prepared. `run_sleep` runs the sleep in its own task, so a client that stops waiting doesn't cut it short | `auth`, `same_secret`, `health`, `version`, `upgrade_*`, `list_models`, `list_sessions`, `board`, `*_session`, `decide`, `DECISIONS`, `list_memory`, `list_skills`, `review_skill`, `review_tool`, `get_policy`, `put_policy`, `undo_policy`, `run_sleep`, `mcp_status`, `list_jobs`, `create_job`, `update_job`, `delete_job`, `run_job`, `list_job_runs`, `handle_socket` | `turns` (start/abort), `memory`, `workshop`, `delegate`, `jobs`, `assist` (`settle`), `mcp`, `score`, `workers`, `update` | router in `main.rs` |
| `src/turns.rs` | 588 | A turn's lifecycle: start (owner or kernel), compile what it sends (tools and instructions from `agent::specs` / `agent::system_for` by the session's kind), record it in `turns`, finish, abort, watchdog, child sessions (`run_child`: a verifier or a subagent, optionally on another model, inheriting the parent's taint; returns the child's id, stops it after 30 minutes). `Turn.kind` is read once at start. A summary made inline at the hard limit is `hold`-counted as a running tool so the watchdog waits, and an abort stops it. `record_turn` returns the row plus the session's parent and cost in one query | `Turn`, `Origin`, `Recorded`, `start_turn`, `run_kernel_session` (a parentless kernel session: the sleep compacting a prompt file), `run_and_wait`, `begin_turn`, `still_ours`, `hold`, `finish_turn`, `abort_turn`, `run_child`, `watchdog`, `record_turn`, `session_cost` | `agent`, `compile`, `compact`, `measure`, `tape`, `workers`, `assist` (`after_turn`) | `api`, `dispatch`, `agent`, `workers`, `delegate`, `memory`, `jobs` |
| `src/dispatch.rs` | 299 | Handles every message from workers (notifications in order per session, one queue task per session; tool calls concurrently): `tool.call` (refused after a tool ended the turn, `agent::refuse`; a verifier may run only `bash`, `read`, `submit_verdict`; then `agent::run_tool` or `tools::execute`), `turn.delta`/`turn.thinking`, `turn.message` (tape + `model_calls`), `turn.fallback` (a `failover` tape block and a `status` event), `turn.usage`, `turn.end`. Before a tool runs: `agent::refusal` by kind, `agent::prompt_file_refusal`; a prompt file is backed up (`memory::backup_file`) before an `edit`/`write` to it and `agent::over_cap_note` added when it ends over its cap. Drops messages not from the turn running on that worker (matched by echoed `turn_id`; by session and worker when a message has none). Attaches newly met AGENTS.md files to tool results | `dispatch`, `handle_incoming`, `touch`, `turn_id`, `session_id` | `agent`, `tools`, `compact`, `context`, `turns`, `memory` (`backup_file`) | `main.rs` (spawned task) |
| `src/agent.rs` | 555 | The agent's tools beyond files and the shell, in a fixed order after them (see "Tools the model gets"); runs `ask` (questions block; ends the turn unless `wait: false`), `verify` (kernel runs criteria commands, a child verifier session judges the rest; `verification` block), `decide` (when a System One model is set; logged as `point = 'tool'`) and the verifier's `submit_verdict`; hands `history`, `search`, `remember`, `capture`, `web_*`, `find_skills`/`load_skill`, `save_skill`/`save_tool`, `find_tools`/`load_tool`/`call_tool`, `delegate` and `schedule` to their modules. A subagent or job session gets no `ask`, `delegate` or `schedule` (`refusal` enforces it); a subagent gets `delegate::system_for` instructions. `prompt_file_refusal`: `edit`/`write` on SOUL, IDENTITY, AGENTS or USER refused for any session with a kind (subagents, verifiers, jobs, kernel sessions) and tainted sessions (D-045); `over_cap_note` asks the agent to compact a file its edit took over its cap. Replaced `flow.rs`: no workflow (docs/brief.md) | `specs`, `system_for`, `read_only`, `run_tool`, `refuse`, `refusal`, `prompt_file_target`, `prompt_file_refusal`, `over_cap_note`, `log_decision`, `combine`, `render_results`, `VERIFIER` | `tools` (`run_shell` for checks), `memory`, `search`, `wiki`, `skills`, `workshop`, `delegate`, `jobs`, `web`, `mcp`, `compact`, `compile` (`file_cap`), `git`, `secrets`, `score`, `turns` (`run_child`); `steps/verify.md` | `dispatch`, `turns` |
| `src/memory.rs` | 869 | Short-term memory and the sleep (DESIGN.md "Memory and skills"): the `remember` tool (what a tainted session saves counts as `inferred`), rendering into the instructions, export to `~/.zenbot/global/MEMORY.md`, the hard ceiling (refuse + start a sleep), `sleep` (promote lasting entries: `belongs_in`, `promote_to_file` into `USER.md`/`IDENTITY.md` under `## Learned`, `wiki::capture_memory`; `compact_file` for a prompt file over its cap through `turns::run_kernel_session` (a `subagent`-kind session with no parent), `compacted_text` checks the answer; `backup_file` into `backups/prompt-files/` (also before an agent's edit, from `dispatch`), pruned at 90 days; then rank, keep, archive; one at a time), the morning note (`morning_note`); removing saved outputs older than 30 days; the sleep also commits the agent's own wiki edits and adds wiki lint problems to its note (when the wiki is a git repo), tends skills (`workshop::tend`) and tunes the routing policy (`delegate::tune`). | `cap`, `render`, `export`, `spec`, `run_tool`, `sleep`, `belongs_in`, `with_learned`, `promote_to_file`, `wilson_lower` (also model routing), `plan`, `priority`, `questions`, `morning_note`, `last_run`, `list` | `score` (`decide`, `private_ok`), `secrets`, `taint::tainted`, `wiki` (`commit`, `lint`, `capture_memory`), `workshop::tend`, `delegate::tune`, `turns::run_kernel_session` | `agent`, `compile`, `api`, `dispatch` (`backup_file`), `jobs` (the `sleep` system job) |
| `src/skills.rs` | 424 | Skills in `~/.zenbot/global/skills/<domain>/<name>/` (`ZEN_SKILLS_DIR`): frontmatter, validation (agentskills.io rules; a description can't contain `<` or `>`), scan (folders starting with `.` or `_` aren't domains), the index for the instructions (active skills only), `find_skills` word matching, `load_skill` (SKILL.md or a file inside the skill, cut at 48 KB). Drafts (`_proposed/<domain>/<name>`, `drafts()`) are found and loaded too, their description prefixed `(draft)` | `root`, `frontmatter`, `validate`, `scan`, `drafts`, `index_text`, `find`, `lookup`, `load`, `find_spec`, `load_spec`, `run_tool` | — | `agent`, `compile`, `workshop` |
| `src/defaults.rs` | 74 | Writes the default prompt files and skills (`defaults/`, compiled in) that are missing under `~/.zenbot`; never overwrites; a skill only when its whole folder is missing | `install` | — | `main.rs` |
| `src/search.rs` | 624 | Search over past sessions, memories and the wiki. `index_loop` (every `ZEN_INDEX_SECS`, sooner while there's a backlog) keeps `search_docs` current: one document per turn of every non-verifier session (the owner's message, the answers and tool calls, not tool output; tape events taken once 5 seconds old, watermark `tape` in `search_state`), one per short-term memory and old `long` row (archived ones removed; watermark `memories`), one per wiki page (`kind = 'wiki'`, `ref` = the page's slug; by each page's file time against the time it was indexed, reading only changed pages; deleted pages removed); then embeddings through an OpenAI-compatible `/embeddings` (`ZEN_EMBED_URL`, default OpenRouter; `ZEN_EMBED_MODEL`, 1536 dimensions; rows of another model re-embedded) when `OPENROUTER_API_KEY` is set, `ZEN_EMBED` isn't `0` and `ZEN_S1_PRIVATE` allows it. A query: exact names and paths first (trigram over `ident`), then full text and vectors of the current model merged by reciprocal rank fusion; System One reranks (`search_rerank` decision, only with `private_ok`). The `search` tool (scope `all` = sessions, memories and wiki; `sessions`, `memories`, `wiki`, `this_session`) indexes first, marks returned memories as used, logs every search in `searches`, and wraps the results as untrusted and taints the session when a returned turn comes from a tainted session | `index_loop`, `index_once`, `refresh_wiki`, `identifiers`, `turn_text`, `exact_candidate`, `query`, `Found`, `spec`, `run_tool` | `score` (`decide`, `private_ok`), `agent::log_decision`, `wiki` (`root`, `page_times`, `documents_where`), `taint` (`tainted`, `taint`, `untrusted`) | `agent`, `wiki` (`index_once`, `query` for candidates), `main.rs` |
| `src/wiki.rs` | 462 | The wiki: markdown pages in `ZEN_WIKI_DIR` (default `<zen home>/global/wiki`), a git repository (`ensure` runs `git init`, adds `index.md` and `log.md`). A page: frontmatter (type: concept, entity, decision, playbook, project, person; aliases), a title, a summary above `---`, then a dated timeline, newest first. The `capture` tool: masks secrets in the note, gathers candidate pages (exact title or alias, then `search` over `wiki`), System One picks a page or `new` and judges `known` and `sensitive` (`capture` decision; only with `private_ok`; without it an exact title match or a new page), skips a known note (≥ 0.8), refuses a sensitive one (≥ 0.8), appends `- **date** \| from, session id (source) — note` (source `web` when the session is tainted), rewrites `index.md`, appends to `log.md`, commits as `zenbot`. The agent rewrites summaries itself with `edit`. `lint`: pages with entries and no summary, links to missing pages | `root`, `slug`, `Page`, `new_page`, `parse`, `add_entry`, `index_text`, `lint`, `ensure`, `commit`, `capture_memory` (the sleep copies lasting knowledge into the wiki), `spec`, `run_tool`, `page_times`, `documents_where` | `search` (`index_once`, `query`), `score` (`decide`, `private_ok`), `agent::log_decision`, `secrets`, `taint::tainted`, git | `agent`, `search` (indexer), `memory` (sleep) |
| `src/workshop.rs` | 616 | The agent improves its skills and makes tools. `save_skill` (domain, name, description, body, optional `files`, a required `reason`): name rules, SKILL.md under 10,000 characters, validated in a scratch copy first; updating an active skill applies at once, an existing draft is updated; a new skill is first checked against the same domain's skills (System One `skill_duplicate` decision, refused at ≥ 0.8; without System One or private content, word overlap ≥ 0.6) and saved as a draft in `skills/_proposed`. A draft becomes active when the owner accepts it (`decide_draft`) or when the owner's `accept` verdict on a session that loaded it arrives (`on_accept`, from `api::decide`); a draft in a new domain needs the owner. Every change is a commit in the skills folder (made a git repo on first commit). `tend` (in the sleep): active skills unused 30 days (since the last load or the SKILL.md change) are flagged in the note, at 90 moved to `skills/_archived`. `save_tool`: `tool.json` (name, description, parameters, command) and its files in `ZEN_TOOLS_DIR`/`<name>`, a `made_tools` row, a commit in the tools folder. `run_made` (from `mcp::call_tool` as `made_<name>`): JSON arguments on stdin, a clean environment (PATH, HOME, USER, LANG, LC_ALL, TZ), 120 s limit, output cut at 50,000 characters and masked; unapproved, or changed since approval (its SHA-256 over manifest and files, computed in Postgres, differs from `approved_sha`): `bwrap` with `/` read-only, empty home, no network; approved and unchanged: `bash -c` unsandboxed; rejected: refused. `stats` (loads, last load, accepted sessions per skill; the tools) | `save_skill_spec`, `save_tool_spec`, `run_tool`, `decide_draft`, `on_accept`, `stats`, `tend`, `made_tools`, `made_entries`, `decide_tool`, `run_made`, `commit`, `overlap`, `tools_root` | `skills`, `score` (`decide`, `private_ok`), `agent::log_decision`, `secrets`, git, bubblewrap | `agent`, `api`, `mcp`, `memory` (sleep) |
| `src/delegate.rs` | 309 | Delegation and model choice. `delegate` takes a `task` or up to 8 `tasks` (each with optional `model`, `dir`), run at once (`join_all`); each is a child session of kind `subagent` via `turns::run_child` (1,900 s limit), and the parent gets each one's final answer with its model, kind, time and cost (wrapped as untrusted, tainting the parent, when the subagent is tainted). A subagent can't delegate (refused) or ask (not offered). Model choice when none is named: System One classifies the kind of work (`kind_of`: understand, shape, bet, build, verify, maintain, reflect, reach; `unknown` without System One or private content), `pick` follows the policy's route for that kind (else `default`, else the parent's model; only served models) and explores a candidate with probability `explore` (policy, else `ZEN_EXPLORE`, capped at 0.5). Each choice is a `model` decision (`session_id` = the child; input: kind, task, parent, how = asked/policy/explore; its probability). `policy` / `set_policy` read and version the `policies` table; `stats` (per kind and model: subtasks, judged and accepted by the owner's verdict on the parent, mean cost); `suggest` / `tune` (in the sleep) switch a route only when another model's Wilson lower bound beats the current one's upper bound, each with at least `ZEN_POLICY_MIN_JUDGED` judged subtasks; the old model stays a candidate | `SUBAGENT`, `system_for`, `policy`, `set_policy`, `pick`, `final_answer`, `spec`, `run_tool`, `stats`, `suggest`, `tune` | `turns::run_child`, `workers` (`collect_models`, routes), `score` (`decide`, `questions`, `private_ok`), `agent::log_decision`, `taint` (`tainted`, `taint`, `untrusted`), `memory::wilson_lower`, `tape` | `agent`, `api`, `memory` (sleep) |
| `src/jobs.rs` | 1062 | Scheduled jobs (D-046): schedules (`parse_schedule`: cron via `croner` in the job's `chrono-tz` zone, `every`, `at`, `in`; `next_after`, `period`, `grace`), the store (`create`, `update`, `set_enabled`, `remove`, `run_now`, `list`, `runs`), the gate for the agent's jobs (System One on the owner's own messages, `ZEN_JOB_BAR`; `job_approval` decisions), `seed` (system jobs `sleep`, `engines`), `run_loop` (marks unfinished runs `interrupted`, sleeps until the earliest due job, `tick` claims with `FOR UPDATE SKIP LOCKED`, skips runs past their grace as `missed`), runs (`run_system`: `memory::sleep` or `update-engines.sh` on `run_shell`; `run_agent`: a session of kind `job` with a `base` block from `compile::compose` + skills + `<scheduled_job>` note, `turns::run_and_wait`; report, `[SILENT]`, `[FAILED]`), `finish` (backoff, pause after 5 failures), the `schedule` tool | `parse_schedule`, `next_after`, `period`, `grace`, `create`, `update`, `set_enabled`, `remove`, `run_now`, `list`, `runs`, `gate`, `seed`, `run_loop`, `tick`, `run_system`, `run_agent`, `finish`, `spec`, `run_tool`, `JOB` | `compile`, `skills`, `memory`, `turns`, `score`, `taint`, `tools` | `main.rs` (`run_loop`), `agent.rs` (tool), `api.rs` |
| `src/web.rs` | 741 | `web_search` and `web_fetch`. Fetch: http(s) to public addresses only (own DNS resolver drops non-public addresses; IP literals and each of at most 5 redirects checked; no proxy), 10 s connect / 30 s / 5 MB, HTML to markdown (`dom_smoothie`, `htmd` fallback; Latin-1/Windows-1252 pages decoded by their charset), 20,000 characters per call paged by `offset`, 15-minute / 32-entry cache (oldest evicted), PDFs saved to `~/.zenbot/outputs/web-*.pdf` for `pdftotext`, `focus` keeps the parts System One judges relevant (`web_focus` decision). Search: Brave or Tavily when a key is set (or `ZEN_SEARCH_PROVIDER`), else SearXNG, which also rescues one failed keyed call; System One reranks (`web_rerank` decision). Every result is wrapped in an `<untrusted>` envelope and taints the session (`sessions.tainted_at`, a `taint` tape block) | `is_public`, `check_url`, `readable`, `chunks`, `render_hits`, `search_spec`, `fetch_spec`, `run_tool` | `score` (`decide`, `private_ok`), `agent::log_decision`, `secrets`, `tape`, `taint` (`taint`, `untrusted`) | `agent`, `memory`, `wiki`, `search`, `mcp` |
| `src/taint.rs` | 78 | Cross-cutting handling of untrusted external content (web, MCP, subagents, history): marker defusing and one `<untrusted>` envelope, session taint (`sessions.tainted_at` plus tape block), and centralized tainted-state lookups | `untrusted`, `taint`, `tainted`, `any_tainted`, `prefix_tainted` | `tape` | `web`, `mcp`, `search`, `wiki`, `memory`, `delegate`, `agent`, `workshop` |
| `src/mcp.rs` | 694 | MCP client, written against the spec (2025-06-18; no `rmcp`): servers from `~/.zenbot/mcp.json` (`ZEN_MCP_CONFIG`; `mcpServers`, `command`/`args`/`env` for stdio or `url`/`headers` for streamable HTTP, `${VAR}` from the kernel's environment; per server `enabled`, `timeout_s`, `include`/`exclude`, `untrusted`, default true for remote). Config re-read when its mtime changes; servers connected lazily, reconnected after a transport error, and retried at most once a minute while down; a bare tool name must be unique. The model's tool list stays fixed: `find_tools` (word ranking, names as `<server>_<tool>`), `load_tool` (schema as a tool result), `call_tool` (required arguments checked, output masked, over 50 KB cut with the full text in `~/.zenbot/outputs/mcp-*.txt`; untrusted servers' output wrapped and the session tainted). Untrusted descriptions and schemas are wrapped and taint the session; the registry lock is not held during server calls. The catalog also lists the agent's own tools as server `made` (`workshop::made_entries`); calling one runs `workshop::run_made` | `config_path`, `expand`, `parse_config`, `sse_messages`, `rank`, `missing_args`, `result_text`, `find_spec`, `load_spec`, `call_spec`, `run_tool`, `status` | `taint::untrusted`, `secrets`, `tape`, `workshop` | `agent`, `api` (`/api/mcp`) |
| `src/tools.rs` | 827 | The file and shell tools and their execution: `bash` on `run_shell` (own process group killed on timeout or abort; the kernel's secret variables removed from the shell's environment (`secrets::is_secret_var`); read-only `bwrap` sandbox for a verifier, with the secret files (`secrets::secret_files`) hidden; also used by `verify`'s criteria checks and the `engines` job), `read` (16 MiB file cap), `write`, `edit` (whitespace/line-ending normalized matching without adding a trailing newline; mixed line endings kept); `write` and `edit` replace files atomically (temp file and rename, symlinks followed, permissions kept). Per-file locks. Output over 50 KB cut to head and tail (off the runtime threads), full text saved under `~/.zenbot/outputs/`. Results are masked at the dispatcher | `specs`, `execute`, `run_shell`, `Shell`, `GroupKill`, `resolve`, `save_full_output`, `ToolOutput` | `secrets` | `dispatch`, `agent`, `jobs`, `context`, `web`/`mcp` (`ToolOutput`) |
| `src/compile.rs` | 321 | What a turn sends, in cache order: envelope (system prompt + tools, stored once in `envelopes`) → summary → history → prompt with turn context. The system prompt: SOUL.md, IDENTITY.md, AGENTS.md (placeholders filled), USER.md (each capped, cut in the middle), memory and the sleep note, the skills index, project instruction files | `system_prompt`, `file_cap`, `cut_middle`, `PARTS`, `compose` (only the parts a scheduled job picks), `base_prompt`, `envelope`, `turn_context`, `history`, `Envelope`, `SummaryRef` | `context`, `memory`, `skills`, `tape` | `turns`, `measure`, `jobs`, `agent` (`file_cap`) |
| `src/context.rs` | 166 | Instruction files (AGENTS.md, else CLAUDE.md, capped at 32 KB): always one per directory from `/` to the workspace (`~/.zenbot/AGENTS.md` is a prompt file, compile.rs); on demand, a project's file the first time a tool touches a path in it (excluding paths under the zen home) (tape kind `context`) | `always`, `governing`, `paths_in_call`, `attachment`, `read_capped` | `tools::resolve` | `compile`, `dispatch` |
| `src/compact.rs` | 482 | Summaries of older turns: linear-time plan, prepared in the background past the soft limit, applied after a pause or at once past the hard limit (`compaction` block). Also the `history` tool, which reads old blocks back by number or search, in this session or another (`session`: a full id or a unique prefix, e.g. the 8 characters `search` shows) | `Settings`, `summary_model`, `plan`, `prepare`, `pending`, `apply`, `tool_spec`, `history_tool` | `tape`, `workers::complete` | `turns`, `dispatch`, `tools` |
| `src/measure.rs` | 177 | Per-turn record of what was sent and why the prompt cache could or couldn't be reused (`cache_break`: `first`, `instructions`, `summary`, `model`, `engine_session`, `expired`; unexpected: `history`, `miss`) | `previous`, `record`, `break_at_start`, `break_at_end`, `cache_ttl_secs` | `compile` types | `turns` |
| `src/tape.rs` | 70 | The tape: append (advisory lock per session, `seq` + parent + hash computed in SQL by `zen_block_hash`), load, repair (`zen_rechain`) | `Block`, `append`, `load`, `load_all`, `repair` | DB functions from migration 0007 | `compile`, `compact`, `agent`, `dispatch`, `turns`, `jobs`, `taint`, `assist`, `main.rs` |
| `src/workers.rs` | 232 | Worker configs from `ZEN_WORKERS` (empty means `engine`), supervision with backoff (ends orphaned turns on a crash), model routing and metadata rebuilt on refresh, all worker models visible by default (`ZEN_MODELS` may curate), effort checks, `complete` for summaries | `Worker`, `worker_configs`, `forward_worker`, `supervise`, `complete`, `visible_models`, `collect_models`, `worker_for`, `model_info`, `check_effort` | `mind`, `score` | `main.rs`, `api`, `turns`, `agent`, `compact`, `assist`, `delegate` |
| `src/mind.rs` | 157 | JSON-RPC client for one worker process (`bash -lc <cmd>`, without the kernel's own credentials: `ZEN_TOKEN`, `DATABASE_URL`, OpenRouter and search keys); 30 s default request timeout, `request_within` for longer; a request's entry is removed however it ends; lines are read as bytes (invalid UTF-8 can't stop the reader) | `Mind`, `Incoming`, `spawn`, `request`, `request_within`, `respond` | — | `workers`, `main.rs` |
| `src/score.rs` | 416 | Direct OpenRouter System One call and live scoring: a classifier answers fixed, versioned questions (`QUESTIONS_VERSION = "v1"`) about a session after a decision or after it goes idle; stored in `session_scores`. Off unless `ZEN_S1_MODEL` is set. Also `decide` (any typed question to System One), `judge` (`Relevance`: reranking and `focus`, logged under their own `point`) and `private_ok` (`ZEN_S1_PRIVATE`) | `scorer`, `openrouter_key`, `decide`, `judge`, `private_ok`, `questions`, `state`, `score_session`, `idle_loop` | OpenRouter `/api/v1/systemone` | `api`, `agent`, `memory`, `web`, `search`, `wiki`, `workshop`, `delegate`, `jobs`, `workers`, `main.rs` |
| `src/secrets.rs` | 242 | Masks secrets in tool output: values of the kernel's own secret-looking env vars, `~/.zenbot/token`, `~/.codex/auth.json` and `~/.claude/.credentials.json` values (reloaded when those files change), and well-known token prefixes / private key blocks | `mask`, `mask_off_thread` (large text on a blocking thread), `is_secret_var` (named like a token, key, secret or password, or `DATABASE_URL`: masked and kept out of the agent's shells), `secret_files` (`token`, `env` under the zen home: hidden from sandboxed shells) | — | `tools`, `agent` (check output, diff), `memory` (memories), `web` (pages), `mcp` (tool output) |
| `src/test_util.rs` | 28 | Test-only owner-private scratch directory removed on drop; `bash` large-output tests save there instead of the live zen home | `TestDir` | — | kernel unit tests |
| `src/update.rs` | 178 | Self-update: compares `~/.zenbot/version` with `origin/main` (hourly by default), asks `scripts/fetch-release.sh --check` whether binaries exist (30-second timeout), starts `scripts/self-update.sh` on request | `Updater` (`running`, `info`, `check`, `check_periodically`, `start`, `status`) | `git`, scripts | `api`, `main.rs` |
| `src/git.rs` | 62 | Async git with a 60 s limit and an output cap (git is stopped once the cap is reached) | `output`, `git` | — | `update`, `agent` (the verifier's diff) |
| `steps/verify.md` | 9 | The verifier's whole system prompt (`include_str!` in `agent.rs`) | — | — | `agent::system_for` |
| `src/layout.rs` | 116 | Where files live under the zen home, by scope (D-040): system-wide prompt files at the top, `agents/<name>/` (the agent's `SOUL.md` and `IDENTITY.md`), `global/` (`MEMORY.md`, wiki, skills, tools). `migrate` moves an old flat layout once at startup, before the defaults, never overwriting, and leaves a relative symlink at each old path for rollbacks | `SOUL`, `IDENTITY`, `global_dir`, `migrate`, `MOVES` | — | `main.rs`, `compile`, `defaults`, `memory`, `wiki`, `skills`, `workshop` |
| `defaults/` | — | Default `SOUL.md`, `IDENTITY.md`, `AGENTS.default.md` (installed as `AGENTS.md`; named so sessions in the repo don't load it as the project's instructions; with `{{workspace}}`, `{{home}}`, `{{zen_home}}`, `{{repo}}`), `USER.md`, and the skills `work/brief` (with `references/template.md`), `work/verify` and `work/close` (keep what a job taught: memory, wiki, skills, tools) (`include_str!` in `defaults.rs`) | — | — | `defaults::install` |
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
  redirect; `taint.rs` wraps web and other external content in one `<untrusted>` envelope (look-alike
  markers defused) and taints the session, so what that session saves to memory counts as inference.
- `mcp.rs`: MCP tools never join the model's tool list (that would break the prompt cache); they
  are reached through `find_tools` / `load_tool` / `call_tool`, and every call goes through the
  kernel like any other tool call.
- `secrets.rs`: masking applies to what tools return; the command text the model typed is not masked.

---

## Kernel routes

From `main.rs` (router) and `api.rs`. Everything under `/api` needs the token in `Authorization: Bearer` (never in the URL).

| Method | Path | Handler | Does |
|---|---|---|---|
| GET | `/health` | `health` | No auth. `{ok, db, mind, workers:{name:bool}, busy, version, commit}`; pings every worker. `busy` = running turns + kernel work outside them. Used by `wait_healthy` and `apply-upgrade.sh` (`"busy":0`) |
| GET | `/api/models` | `list_models` | Asks every worker for `models.list`, refreshes routes, returns all worker models by default (`ZEN_MODELS` optionally curates, plus any `faux/*`), `authenticated`, `default`, `scorer` |
| GET | `/api/sessions?archived=` | `list_sessions` | The owner's sessions and scheduled jobs' sessions (`kind IS NULL OR kind = 'job'`; subagents, verifiers and kernel sessions hidden), with cost |
| GET | `/api/board` | `board` | Every session (archived and child ones too), newest activity first, with `kind`, `parent`, `busy`, `waiting` (unanswered `ask` questions) and, for child sessions, `task` (first message); no cost. zen's sessions board polls it |
| POST | `/api/sessions` | `create_session` | `{title?, model?, effort?}` |
| GET | `/api/sessions/{id}?last=&after=` | `get_session` | Session, its `message` blocks (with `seq`; all, the `last` N, or those after `after`) and `busy` |
| PATCH | `/api/sessions/{id}` | `update_session` | Title, model, effort (`"default"` clears it), archived |
| GET | `/api/sessions/{id}/ws` | `session_ws` | WebSocket; its session's event hub is created on connect and freed when the last client leaves. Client sends `{type:"prompt",text}` or `{type:"abort"}`. A `prompt` may carry `suggestion: {id, taken}` (what the owner did with the suggested prompt). Server events: `message`, `delta`, `thinking`, `tool_start`, `tool_end`, `busy`, `end`, `status`, `questions`, `child_end`, `title`, `suggestion`, `error`, `resync` |
| POST | `/api/sessions/{id}/decision` | `decide` | `{decision: accept\|more\|reshape\|drop, note?}` → `session_decisions`; triggers scoring; `accept` activates draft skills the session loaded (`workshop::on_accept`) |
| GET | `/api/memory?tier=` | `list_memory` | Memories of a tier (`short` by default, `archived`, `all`; `long` and `proposed` only hold rows from before D-045), the last sleep and the size |
| POST | `/api/memory/sleep?trigger=` | `run_sleep` | Tidy short-term memory now (`trigger=nightly` is recorded as the nightly sleep, else `owner`; the scheduled `sleep` job calls `memory::sleep` directly) |
| GET | `/api/suggestions` | `assist::stats` | Suggested next prompts: outcomes (`accepted`, `edited`, `declined`, `unseen`, open) by prompt version and model, latency and cost; the latest 20 |
| GET/POST | `/api/jobs` | `list_jobs` / `create_job` | Every job (`jobs`, `scheduler` on/off); POST `{name, prompt, schedule, tz?, context?, skills?, model?, workspace?}` creates an agent job as the owner |
| PATCH/DELETE | `/api/jobs/{name}` | `update_job` / `delete_job` | `{enabled}` pauses or resumes (resume approves an agent's paused job); other fields change an agent job; DELETE removes an agent job (runs kept) |
| POST | `/api/jobs/{name}/run` | `run_job` | Run a job now in the background (`{run}`; 400 when it's already running) |
| GET | `/api/jobs/runs?job=&limit=` | `list_job_runs` | Recent runs, newest first: status, trigger, session, report or error |
| GET | `/api/mcp` | `mcp_status` | `{config, servers:{name:tool count}, problems}` from `~/.zenbot/mcp.json` (the agent's own tools count as server `made`); connects servers not yet connected |
| GET | `/api/policy` | `get_policy` | The routing policy in force (`version`, `policy`), the evidence (`stats`) and what it suggests |
| POST | `/api/policy` | `put_policy` | `{policy: {routes, explore?}, reason?}`: a new version by the owner |
| POST | `/api/policy/undo` | `undo_policy` | A new version with the data of the one before the latest (409 when there is none) |
| GET | `/api/skills` | `list_skills` | Skills (active and drafts) with loads, last load, accepted and judged sessions that loaded them; the agent's tools and whether each is approved |
| POST | `/api/skills/review` | `review_skill` | `{name: "domain/name", decision: accept\|reject}`: activate a draft skill or move it to `_archived` |
| POST | `/api/tools/{name}/review` | `review_tool` | `{decision}`: `accept` approves a made tool (network, no sandbox); anything else rejects it |
| GET | `/api/version?refresh=` | `version` | Running commit vs `origin/main` |
| GET/POST | `/api/upgrade` | `upgrade_status` / `upgrade_start` | Start `scripts/self-update.sh`; status = job + last line of `~/.zenbot/upgrade.log` |

---

## Tools the model gets

`tools.rs::specs()` (`bash`, `read`, `write`, `edit`) then `agent.rs::specs()` adds the rest, in a
fixed order, the same every turn: `bash`, `read`, `write`, `edit`, `history`, `search`, `ask`, `remember`,
`capture`, `web_search`, `web_fetch`, `find_skills`, `load_skill`, `save_skill`, `find_tools`, `load_tool`, `call_tool`,
`save_tool`, `verify`, `delegate`, `schedule`, then `decide` when System One is configured and may see private content. A subagent session (`kind = 'subagent'`) and a scheduled job's session (`kind = 'job'`) get the same list without `ask`, `delegate` and `schedule`. A verifier session (`kind = 'verifier'`)
gets `bash`, `read`, `submit_verdict` only (`dispatch.rs` refuses anything else). There are no
session states and no state-dependent tools.

| Tool | Executed by | Offered to |
|---|---|---|
| `bash`, `read`, `write`, `edit` | `tools.rs` (bash in a read-only bwrap sandbox for a verifier) | every session (a verifier: `bash`, `read`) |
| `history` | `compact.rs::history_tool` (this session, or another by `session`) | all but verifiers |
| `search` | `search.rs` (past sessions, memories and wiki pages) | all but verifiers |
| `ask` | `agent.rs` (`questions` block and event; ends the turn unless `wait: false`) | the owner's sessions (not subagents, jobs or verifiers) |
| `remember` | `memory.rs` | all but verifiers |
| `capture` | `wiki.rs` (a dated entry on a wiki page, committed in git) | all but verifiers |
| `web_search`, `web_fetch` | `web.rs` (wrapping and session taint in `taint.rs`) | all but verifiers |
| `find_skills`, `load_skill` | `skills.rs` (active skills and drafts) | all but verifiers |
| `save_skill` | `workshop.rs` (create or improve a skill; new ones are drafts) | all but verifiers |
| `find_tools`, `load_tool`, `call_tool` | `mcp.rs` (the owner's MCP servers, and the agent's own tools as `made_<name>`, run by `workshop::run_made`) | all but verifiers |
| `save_tool` | `workshop.rs` (make or update a tool in `ZEN_TOOLS_DIR`) | all but verifiers |
| `verify` | `agent.rs` (criteria commands on `run_shell`; a child verifier via `turns::run_child`; `verification` block) | all but verifiers |
| `delegate` | `delegate.rs` (subagent sessions, in parallel for `tasks`; model by the routing policy) | the owner's sessions (not subagents, jobs or verifiers) |
| `schedule` | `jobs.rs` (create, update, list, runs, pause, resume, remove, run; an agent's job is live only past the gate) | the owner's sessions (not subagents, jobs or verifiers) |
| `decide` | `agent.rs` → `score.rs` → OpenRouter `/systemone`, logged in `decisions` (`point = 'tool'`) | when `ZEN_S1_MODEL` is set, `ZEN_S1_PRIVATE` isn't `0` and `ZEN_DECIDE_TOOL` isn't `0` |
| `submit_verdict` | `agent.rs` (`verdict` block; ends the turn) | verifiers only |

`sessions.state` is no longer written or returned (old sessions keep theirs; see Notes).

---

## Layer 2 — Workers

Protocol: `docs/worker-protocol.md`. Kernel → worker: `ping`, `models.list`, `turn.start`,
`turn.abort`, `complete`. Worker → kernel: `tool.call` (request) and the
notifications `turn.delta`, `turn.thinking`, `turn.message`, `turn.fallback` (a quota failover switched provider), `turn.usage`, `turn.end`.
`turn.start` carries the kernel's `turn_id`; workers echo it on every message of the turn and key
running turns by it (not by session), since the session's next turn can start while an ended one is
still stopping. `turn.abort` takes an optional `turn_id` (without it: every turn of the session).
`turn.usage` may carry `fallback_reason` (resume or native history refused).
**A protocol change must update the doc and every worker** (AGENTS.md).

### `zen-engine` (`crates/zen-engine`, ~2,160 lines)

| File | Lines | Role | Depends on | Used by |
|---|---|---|---|---|
| `src/main.rs` | 153 | Entry: `zen-engine` serves the protocol on stdio; `zen-engine mcp-bridge <socket>` runs the bridge. Routes `turn.start`/`complete` by model prefix, makes one safe cross-provider quota continuation when eligible; running turns keyed by `turn_id` | all modules | `zend` (`ZEN_ENGINE_CMD`), Claude CLI (bridge) |
| `src/rpc.rs` | 74 | JSON-RPC over stdio with the kernel | — | everything |
| `src/turn.rs` | 562 | Shared per-turn pieces: `TurnInput`, `TurnCtx` (session and turn ids, transcript and usage capture, in-flight tool safety tracking; a Unix socket per turn forwards tools to the kernel as `tool.call`), `StderrTail` (last few KB of a CLI's stderr, quoted in errors), history transcript, seed blocks, `engine_dir` (private folders under `<zen home>/engine/`: `claude`, `codex`, `complete`, `sockets`), `enabled(var)` (on unless `"0"`) | `rpc`, `zen-proto` | `claude`, `codex`, `faux` |
| `src/bridge.rs` | 70 | The stdio MCP server Claude Code launches; answers `tools/list` and `tools/call` by asking the engine over the socket | `turn` socket | Claude CLI |
| `src/claude.rs` | 474 | Runs a turn with `claude -p` (stream-json), `--tools ""`, `--strict-mcp-config`, `--setting-sources ""`, `--system-prompt-file` (zenbot's prompt in a private per-turn file, not on argv), allowed tools `mcp__zen__*`; the turn is refused if `system/init` lists any tool not `mcp__zen__*`. The tool socket lives in a private per-turn folder under the engine home. Engine sessions run in the fixed directory `<zen home>/engine/claude` (Claude Code files them under `$CLAUDE_CONFIG_DIR/projects/`; resume / seed / transcript); unused ones deleted after 30 days. `complete` runs in `<zen home>/engine/complete`. Fails the turn on stream lines it doesn't understand, and reports `fallback_reason` | `turn`, `bridge` | `main.rs` |
| `src/codex.rs` | 478 | Runs a turn through `codex app-server` (cwd: an empty private dir, `<zen home>/engine/codex`): threads kept unless `ZEN_CODEX_RESUME=0`, history injected as native items (`thread/inject_items`, fallback transcript), zenbot tools as dynamic tools; a private `CODEX_HOME` links only the owner's sign-in (from `$CODEX_HOME`, else `~/.codex`). Fails loudly: refuses app-server requests it doesn't handle (ending the turn), errors quote stderr, a turn with no messages fails, `fallback_reason` reported; model listing retried until it succeeds; an abort also stops a hung app-server start or thread setup, and an aborted turn still reports its usage; `codex --version` asked once per process | `turn` | `main.rs` |
| `src/failover.rs` | 238 | One safe cross-provider continuation on a hard subscription quota error (D-044; off with `ZEN_QUOTA_FAILOVER=0`); only on a subscription sign-in (refused when `ANTHROPIC_API_KEY`/`CLAUDE_CODE_OAUTH_TOKEN` or `OPENAI_API_KEY`/`CODEX_ACCESS_TOKEN` is set, so it never bills an API key); validates the target, carries current-turn history without repeating uncertain tool calls, and sends `turn.fallback` | `turn`, `claude`, `codex` | `main.rs` |
| `src/faux.rs` | 107 | Scripted model `faux/smoke` (when `ZEN_FAUX=1`), steps from `ZEN_FAUX_SCRIPT` (list, or object keyed by kind of session: `verify` for a verifier, `default`): `tool`, `text`, `sleep`, `exit`; modifiers `when` (only if the prompt contains the text) and `ignore_abort` | `turn` | e2e, upgrade smoke test, evals |

Code that depends on a CLI's flags or output should fail loudly, so the daily engine update check
(`scripts/update-engines.sh`) catches a breaking CLI release.

---

## Layer 3 — CLI `zen` (`crates/zen`, ~6,700 lines)

| File | Lines | Role | Depends on | Used by |
|---|---|---|---|---|
| `src/main.rs` | 1217 | clap commands (plain `zen` opens the sessions board): `ask`, `chat`, `sessions {ls,new,show,archive,restore,rename,decide}`, `memory [--tier short\|archived\|all] [sleep]`, `jobs [add\|set\|pause\|resume\|rm\|run\|runs]`, `skills [accept\|reject <domain/name>]` (use per skill and the made tools), `tools accept\|reject <name>` (a `made_` prefix is dropped), `policy [set <kind> <model> [--candidates a,b] [--explore x] \| undo]` (the routing policy and its evidence), `suggestions` (what became of suggested prompts), `models`, `login [claude\|codex]`, `status` (with memory, jobs and engines lines), `upgrade [--check]`; flags `--url` (`ZEN_URL`), `--token` (`ZEN_TOKEN`), `--json`, `-c`, `-r`, `-m`, `-e`, `--inline` (`ZEN_INLINE`). Reads `<zen home>/env` (for `ZEN_REPO`, `PATH`) and `<zen home>/engines.json` | `client`, `tui`, `md` | owner, scripts (`zen ask --json`), e2e, evals |
| `src/client.rs` | 332 | HTTP + WebSocket client; `zen_home` (`ZEN_HOME`, else `~/.zenbot`); token from `--token`/`ZEN_TOKEN` or `<zen home>/token`; upgrade wait/poll messages | reqwest, tungstenite | `main.rs`, `tui/` |
| `src/tui/` | ~4,200 | Interactive app split into `mod` (lifecycle), `state` (sessions/reconnect), `events`, `input`, `render`, `transcript` (incremental streaming and view), `board` (the sessions board, zen's home screen), `panel`, `pickers`, `inline`, `files` and `test_util`. Full screen has a diffed, scrollable view and side panel; inline keeps scrollback. Terminal text is sanitized, requests fail into notices rather than exiting, and reconnect rebuilds from the server. Prompt history is private JSON lines in `<zen home>/history`. Render/key tests live beside the modules | `client`, `editor`, `md` | `main.rs` |
| `src/screen.rs` | 124 | Full-screen frames: writes only changed rows in place; `fit` cuts/pads styled lines; `row` joins chat and panel | `md` | `tui/render.rs` |
| `src/editor.rs` | 512 | Multi-line input editor with prompt history | — | `tui/` |
| `src/md.rs` | 277 | Styled lines, word wrap, line-oriented markdown renderer, terminal text sanitizer | — | `tui/`, `main.rs` |

UI changes to `tui/` / `editor.rs` come with render or key tests (AGENTS.md).

## Shared: `zen-proto` (`crates/zen-proto`, 48 lines)

`text_of` (message content as text, in the message format the tape stores), `head`, `tail`. Used
by `zend` (`api`, `agent`, `assist`, `compact`, `delegate`, `jobs`, `mcp`, `memory`, `score`,
`search`, `web`, `wiki`, `workshop`), `zen-engine` (`turn`), `zen` (`main`, `tui`) and `zen-matrix`
(`relay`). Changing how content is read changes it in all four.

---

## Channel: `zen-matrix` (`crates/zen-matrix`, its own Cargo workspace, ~1,200 lines)

A separate process, a client of the client protocol (D-049; docs/matrix.md has the file table).
Not in the root workspace or CI's release tarball: `scripts/matrix.sh` builds and installs it, and
`apply-upgrade.sh` doesn't upgrade it. Settings come from `~/.zenbot/matrix.env` (the process
environment wins; see Environment variables).
`main.rs` (commands `login`/`run`/`status`, the Matrix client, rooms, invites, per-room link tasks,
job reporter), `relay.rs` (pure: commands, events → posts, numbered answers, job reports),
`kernel.rs` (the API calls it uses: `POST /api/sessions`, the session WebSocket, `GET /api/jobs/runs`),
`state.rs` (`matrix.env`, `state.json`, private files). `examples/owner.rs`: the scripted owner for
`scripts/matrix-e2e.sh`. The kernel knows nothing of it.

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
| `sessions` | 0001; `effort` 0003; `state`, `parent`, `kind` 0011; `workspace` 0013; `tainted_at` 0016; `title_source` 0020 (`auto`, `model`, `owner`: the assist model never renames an `owner` title); model ids rewritten 0002 | One per session (one job). `kind` is NULL for the owner's sessions; `'verifier'` or `'subagent'` for child sessions (`parent` set; also `'subagent'` with no parent for a kernel session, e.g. the sleep compacting a prompt file), `'job'` for a scheduled job's run (no parent; listed with the owner's sessions). A child inherits the parent's `tainted_at`, and a subagent may get its own `model`; `workspace` overrides the kernel's; `tainted_at` = when it first read untrusted content (web, untrusted MCP servers); `state` (legacy, no longer written) | `api` (create/update), `turns` (child and kernel sessions), `jobs` (job sessions), `assist` (title, `title_source = 'model'`), `tape::append` (`updated_at`), `taint` (`tainted_at`) | `api`, `turns`, `assist`, `taint::tainted` (many modules), e2e |
| `tape_events` | 0001; `seq`, `parent`, `hash` + functions `zen_block_hash`, `zen_rechain` 0007; index `(session_id, kind, seq)` 0014 | The tape. Kinds: `message`, `base`, `envelope`, `context`, `compaction`, `engine_session`, `questions`, `verdict`, `verification`, `taint`, `failover`; from the old workflow, no longer written: `state`, `brief`, `approval`, `ruling`, `submission`, `report` | `tape::append` only (callers: `dispatch`, `turns`, `jobs`, `compile`, `compact`, `agent`, `taint`) | `tape::load*`, `compact`, e2e (`tape_is_sound`) |
| `model_calls` | 0001; `turn_id`, `duration_ms` 0004; index 0012; partial index on untraced calls (`turn_id IS NULL`) 0014 | One row per assistant message: tokens, cache, cost | `dispatch` (`turn.message`) | `turns`, `measure`, `session_cost` |
| `tool_calls` | 0001; `turn_id` 0004; index 0012 | One row per tool call | `dispatch` (`tool.call`) | `turns`, `workshop` (`load_skill` calls: skill use, `on_accept`) |
| `turns` | 0004; `envelope`, `context`, `context_tokens`, `cache_break` 0009 | One per turn: harness, worker, engine, model, effort, outcome, totals, what was sent | `turns` (`begin_turn` insert, `record_turn` update) | `api`, `measure`, `score`, `turns`, e2e, evals |
| `session_decisions` | 0005; `source` 0011 | The owner's verdict (`accept/more/reshape/drop`; ground truth); `source` was also `model` under the old workflow's auto-close | `api::decide` | `workshop::stats` (accepted sessions per skill), `delegate::stats` (the parent's verdict judges its subagents), e2e (scoring compares against it later) |
| `session_scores` | 0006; index `(turn_id, trigger)` 0014 | System One answers about a session | `score` | `score` |
| `envelopes` | 0008 | System prompt + tools, stored once per distinct pair, keyed by hash | `compile::envelope` | `compile` |
| `compactions` | 0010 | How each summary was made; applied ones have `applied_seq` | `compact` | `compact`, e2e |
| `decisions` | 0011 | System One decisions and what was done, by `point`: `tool` (the `decide` tool), `sleep` (the sleep's fate for each memory), `web_rerank` (web search results ordered), `web_focus` (parts of a page kept), `search_rerank` (`search` results ordered), `capture` (the wiki page a note goes to, whether it's known or sensitive), `skill_duplicate` (whether a new skill does an existing one's job), `model` (a subagent's model: `session_id` is the child, the input names the parent, kind and how), `job_approval` (whether the owner asked for a job the agent creates); older rows from the workflow's shadow decisions. | `agent::log_decision` (from `agent`, `score::judge` for `web` and `search`, `wiki`, `workshop`, `delegate`, `jobs`), `memory::sleep` | `delegate::stats` (`model` points) |
| `policies` | 0011 | The routing policy for subagents, versioned (`version`, `data` `{routes: {kind: {model, candidates}}, explore}`, `reason`, `created_by` `owner` or `sleep`); the latest is in force | `delegate::set_policy` (from `api` and `delegate::tune`), `api::undo_policy` | `delegate`, `api` |
| `memories` | 0015 | Memory entries: text, source (`owner`, `verified`, `inferred`), tier (`short`, `long`, `archived`), use counts, the last sleep's scores, `proposed` (`wiki` once a sleep copied it into the wiki; `promote`/`user` from before D-045), why the tier changed (`reason`) | `memory` (`remember`, `sleep`), `search` (`used_at`, `uses` of returned memories) | `memory`, `api`, `search` (indexer) |
| `sleep_runs` | 0015 | One row per sleep: trigger (`nightly`, `ceiling`, `owner`), scorer, counts, note, error | `memory::sleep` | `memory` (`morning_note`, `last_run`), `api` |
| `search_docs` | 0017 (extensions `vector`, `pg_trgm`) | One searchable document per turn (`kind = 'turn'`, `ref` `<session>:<seq>`), memory (`memory`, `ref` `m<id>`) or wiki page (`wiki`, `ref` = the slug): `ident` (exact names, trigram index), `title`, `body`, generated `tsv` (`simple` config, GIN), `embedding vector(1536)` (HNSW, cosine) + `embed_model`, `at` | `search` (indexer) | `search::query` |
| `search_state` | 0017 | The indexer's watermarks (`tape`: last tape event id; `memories`: last `updated_at`; `wiki`: newest page mtime, in seconds) | `search` | `search` |
| `searches` | 0017 | Every `search` call: session, query, scope, results (`kind`, `ref`, `exact`, `relevance`), `reranked` | `search` | — (to measure search) |
| `jobs` | 0019 | Scheduled jobs (D-046): `kind` `system` (`action` `sleep`, `engines`) or `agent` (`prompt`, `context`, `skills`, `model`, `workspace`); `schedule` and `tz`; `enabled`, `paused_reason`; `created_by` `owner`/`agent`/`kernel`, `created_in`, `approval` (System One's judgment); scheduler state `next_run_at`, `last_*`, `failures`; `removed_at`. Unique `name` among live jobs | `jobs` | `jobs`, `api` |
| `job_runs` | 0019 | One row per run: `trigger` (`schedule`, `catchup`, `owner`, `agent`), `status` (`running`, `ok`, `silent`, `error`, `missed`, `interrupted`), `session_id`, report (`output`) or `error`. A partial unique index allows one `running` run per job | `jobs` | `jobs`, `api` |
| `prompt_suggestions` | 0020 | One row per suggested next prompt: `suggested`, `after_seq`, `model`, `prompt_version`, `latency_ms`, `cost_usd`; when the next prompt arrives, `outcome` (`accepted`, `edited`, `declined`, `unseen`) and `final` (what was sent, unless accepted) | `assist` | `assist` (`stats`) |
| `made_tools` | 0018 | One row per tool the agent made: `approved_at`, `approved_sha` (its content when approved), `rejected_at` (the owner's decision, kept out of the tool's folder the agent can write) | `workshop` (`save_tool` insert, `decide_tool`) | `workshop` (`run_made`, `made_tools`) |

`scripts/db.sh pending` lists migrations the live database hasn't applied; `scripts/db.sh backup`
dumps it to `~/.zenbot/backups/` (last 10 kept).

---

## Environment variables

The service reads `~/.zenbot/env` (systemd `EnvironmentFile`). `install.sh` writes `ZEN_TOKEN`,
`ZEN_PORT`, `ZEN_REPO`, `ZEN_WORKERS`, `HOME`, `PATH` there. Workers inherit
the kernel's environment except `ZEN_TOKEN`, `DATABASE_URL`, `OPENROUTER_API_KEY`, `BRAVE_API_KEY` and
`TAVILY_API_KEY` (`mind.rs`); the agent's shells get none of the kernel's secret-looking variables
(`secrets::is_secret_var`).

### Kernel (`zend`)

| Var | Default | Read in | Effect |
|---|---|---|---|
| `ZEN_TOKEN` | required | `main.rs` | API token; also masked in tool output |
| `DATABASE_URL` | `postgres://zen:zen@127.0.0.1:5432/zen` | `main.rs`, `scripts/db.sh` | Database |
| `ZEN_PORT` | `8100` | `main.rs`, scripts | Listen port |
| `ZEN_BIND` | `127.0.0.1` | `main.rs` | Listen address; opt in to external access explicitly |
| `ZEN_WORKSPACE` | `$HOME` | `main.rs` | Default working directory for tools |
| `ZEN_REPO` | `$HOME/zenbot` | `main.rs`, `zen` CLI | zenbot's checkout: named in the system prompt; used by the updater |
| `ZEN_DEFAULT_MODEL` | `claude/claude-opus-5-5` | `main.rs` | Model for new sessions |
| `ZEN_HARNESS` | `~/.zenbot/version` | `main.rs` | Build id recorded with every turn (dev, smoke and eval kernels set it) |
| `ZEN_WORKERS` | `engine` | `workers.rs`, scripts | Workers to start |
| `ZEN_ENGINE_CMD` | `zen-engine` next to `zend` | `workers.rs` | Command for `engine` |
| `ZEN_WORKER_<NAME>_CMD` | the name | `workers.rs` | Command for any other worker name |
| `ZEN_MODELS` | unset (all worker models) | `workers.rs` | Optional visible model allowlist and order |
| `ZEN_QUOTA_FAILOVER` | `1` | `zen-engine/failover.rs` | Set `0` to disable hard-usage-limit continuation |
| `ZEN_FAILOVER_CLAUDE_TO_CODEX` / `ZEN_FAILOVER_CODEX_TO_CLAUDE` | `codex/gpt-6.1-sol` / `claude/claude-sonnet-5-5` | `zen-engine/failover.rs` | Override cross-provider target model ids |
| `ZEN_HOME` | `$HOME/.zenbot` | `main.rs`, `layout.rs`; also `zen-engine` (`engine/`), `zen` and `zen-matrix` | Prompt files, `agents/`, `global/` (skills, wiki, tools, `MEMORY.md`), `mcp.json`, web PDFs and long MCP output in `outputs/` (dev, eval and smoke kernels set their own) |
| `ZEN_SOUL_CHARS` / `ZEN_IDENTITY_CHARS` / `ZEN_AGENTS_CHARS` / `ZEN_USER_CHARS` | `4000` / `4000` / `12000` / `3000` | `compile.rs` | Size cap of each prompt file in the instructions |
| `ZEN_SKILLS_DIR` | `$ZEN_HOME/global/skills` | `skills.rs` | Where skills live |
| `ZEN_SKILL_INDEX_CHARS` | `2500` | `skills.rs` | Above this the skills index lists domains only |
| `ZEN_PROMOTE_BAR` | `0.9` | `memory.rs` | The sleep's bar for promoting an entry on its own (lowest of three samples for a prompt file) |
| `ZEN_MEMORY_CHARS` | `4000` | `memory.rs` | Short-term memory's size; twice it is the hard ceiling |
| `ZEN_TOOLS_DIR` | `$ZEN_HOME/global/tools` | `workshop.rs` | Where the agent's own tools live (a git repository) |
| `ZEN_WIKI_DIR` | `$ZEN_HOME/global/wiki` | `wiki.rs` | Where the wiki lives (a git repository) |
| `ZEN_INDEX_SECS` | `20` | `search.rs` | Pause between search-index passes when there's no backlog |
| `ZEN_EMBED` | on | `search.rs` | `0`: no embeddings (search by exact names and full text only) |
| `ZEN_EMBED_MODEL` | `openai/text-embedding-3-small` | `search.rs` | Embedding model (must give 1536 dimensions) |
| `ZEN_EMBED_URL` | `https://openrouter.ai/api/v1` | `search.rs` | OpenAI-compatible base URL for `/embeddings` |
| `ZEN_DECIDE_TOOL` | on | `agent.rs` | `0` hides the `decide` tool |
| `ZEN_EXPLORE` | `0.1` | `delegate.rs` | Share of routed subtasks that try a candidate model when the policy sets no `explore` (at most 0.5) |
| `ZEN_POLICY_MIN_JUDGED` | `20` | `delegate.rs`, `api.rs` | Judged subtasks each model needs before the sleep may switch a route |
| `ZEN_JOBS` | on | `jobs.rs` | `0`: the scheduler doesn't run (smoke, e2e, eval and dev kernels set it); system jobs are still seeded |
| `ZEN_JOB_BAR` | `0.9` | `jobs.rs` | System One's probability that the owner asked for a job the agent creates, needed for it to go live |
| `ZEN_TZ` | `America/Sao_Paulo` | `jobs.rs` | Default timezone of a new agent job |
| `ZEN_JOBS_TICK` | `60` | `jobs.rs` | Longest the scheduler sleeps between checks, in seconds (tests make it shorter) |
| `ZEN_S1_MODEL` | unset (off) | `score.rs` | OpenRouter System One classifier: scoring, `decide`, the sleep, reranking, `capture`, delegation, the job gate |
| `ZEN_S1_URL` | `https://openrouter.ai/api/v1/systemone` | `score.rs` | System One endpoint override (primarily for local tests) |
| `ZEN_S1_PRIVATE` | on | `score.rs` | `0`: no private content goes to OpenRouter: no embeddings, no System One reranking or `focus`, no `decide` tool, no sleep judgments, `capture`/`skill_duplicate`/the subagent's kind decided without it (callers of `score::private_ok`) |
| `ZEN_SEARCH_PROVIDER` | `brave` if `BRAVE_API_KEY` is set, else `tavily` if `TAVILY_API_KEY` is, else `searxng` | `web.rs` | Which search provider `web_search` uses |
| `OPENROUTER_API_KEY` | unset | `score.rs`, `search.rs` | Authenticates System One and search embeddings at OpenRouter (kept from workers and shells) |
| `BRAVE_API_KEY` / `TAVILY_API_KEY` | unset | `web.rs` | Keys for Brave / Tavily search (secret-looking, so masked in tool output) |
| `ZEN_SEARXNG_URL` | `http://127.0.0.1:8888` | `web.rs` | SearXNG base URL: the keyless provider and the rescue for a failed keyed call |
| `ZEN_SEARCH_RERANK` | on | `web.rs` | `0`: don't have System One rerank search results |
| `ZEN_MCP_CONFIG` | `$ZEN_HOME/mcp.json` | `mcp.rs` | MCP servers file; `${VAR}` in it filled from the kernel's environment |
| `ZEN_SCORE_IDLE_SECS` | `7200` | `score.rs` | Idle time before a session is scored |
| `ZEN_CONTEXT_TOKENS` | `200000` | `compact.rs` | Context budget (capped by the model's window) |
| `ZEN_COMPACT_SOFT` / `ZEN_COMPACT_HARD` / `ZEN_COMPACT_KEEP` | `0.7` / `0.9` / `0.3` | `compact.rs` | Fractions of the budget: prepare a summary / apply now / keep verbatim |
| `ZEN_COMPACT_IDLE_SECS` | `300` | `compact.rs` | Pause after which a prepared summary is applied |
| `ZEN_ASSIST_MODEL` | `claude/claude-haiku-5-5` | `assist.rs` | Writes session names and suggested next prompts (the cheapest, fastest model on the owner's plans) |
| `ZEN_SUGGEST` | on | `assist.rs` | `off` (or 0/false/no): no suggested next prompts (names stay on) |
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
| `ZEN_FAUX` | off | `faux.rs` | `1` lists the scripted model `faux/smoke` |
| `ZEN_FAUX_SCRIPT` | built-in script | `faux.rs` | JSON file of faux steps |
| `ZEN_CLAUDE_RESUME` | on | `claude.rs` | `0`: every turn in a fresh, unsaved Claude Code session |
| `ZEN_CODEX_RESUME` | on | `codex.rs` | Keep Codex threads across turns (Codex ties its prompt cache to the thread); `0`: a new thread per turn |
| `ZEN_CODEX_INJECT` | on | `codex.rs` | `0`: history as a transcript instead of native items |
| `CLAUDE_CONFIG_DIR` | `~/.claude` | `claude.rs` | Where Claude Code keeps its sessions |
| `CODEX_HOME` | `~/.codex` | `codex.rs` | Where the owner's Codex sign-in is linked from |
| `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` / `OPENAI_API_KEY`, `CODEX_ACCESS_TOKEN` | unset | `failover.rs` | When set, quota failover to that engine is refused (it would bill an API key, not the subscription) |

### CLI and scripts

| Var | Default | Read in | Effect |
|---|---|---|---|
| `ZEN_URL` | `http://127.0.0.1:8100` | `zen` | Kernel URL |
| `ZEN_TOKEN` | `<zen home>/token` | `zen` | API token |
| `ZEN_HOME` | `~/.zenbot` | `zen` | Where `token`, `env`, `engines.json` and `history` are read |
| `ZEN_INLINE` | off | `zen` | Inline terminal app |
| `ZEN_SMOKE_PORT` | `18199` | `upgrade.sh` | Smoke-test kernel port |
| `ZEN_DEV_PORT` / `ZEN_DEV_DB` / `ZEN_DEV_HOME` | `18100` / `zen_dev` / `~/.zenbot-dev` | `dev.sh` | Dev kernel port, database and zenbot home |
| `ZEN_E2E_PORT` / `ZEN_E2E_KEEP` / `ZEN_E2E_NO_BUILD` | `18377` / – / – | `e2e.sh` | e2e kernel port (the test MCP server, SearXNG stub and System One stub use the next three ports); keep DB and files; skip the build (CI) |
| `ZEN_E2E_MCP_TOKEN` | set by `e2e.sh` | `e2e.sh` (`mcp` scenario) | Tests `${VAR}` expansion in `mcp.json` headers |
| `ZEN_MATRIX_E2E_PORT` / `ZEN_MATRIX_E2E_HS_PORT` | `18197` / `16167` | `matrix-e2e.sh` | Faux kernel port; throwaway homeserver port |
| `ZEN_SLOW_SECS` | `12` | `scripts/e2e/slow_worker.py` | How long the test summarizer's `complete` takes |
| `ZEN_EVAL_PORT` / `ZEN_EVAL_KEEP_BUILDS` | `18301` / `5` | `eval.sh` | Eval kernel port; base builds kept |
| `ZEN_ENGINES` / `ZEN_ENGINES_FAIL` / `ZEN_ENGINES_CLAUDE_MODEL` / `ZEN_ENGINES_CODEX_MODEL` | all / – / cheapest listed | `update-engines.sh` | Limit engines; force a failed check (tests rollback); model for the check |
| `ZEN_BUILD_FROM_SOURCE` | – | `fetch-release.sh` (so `install.sh`, `upgrade.sh`) | `1`: never download binaries |
| `ZEN_RELEASE_BASE` | GitHub `edge` release of `origin` | `fetch-release.sh` | Where binaries are downloaded from |

### Matrix channel (`zen-matrix`)

Read from the process environment, else `~/.zenbot/matrix.env` (`state.rs::Config::load`).

| Var | Default | Effect |
|---|---|---|
| `MATRIX_USER` / `MATRIX_OWNER` | required | The bot's Matrix ID; the only account it answers (full `@name:server` IDs) |
| `MATRIX_PASSWORD` | – | First sign-in only (`zen-matrix login`) |
| `MATRIX_HOMESERVER` | from the user ID | Homeserver URL |
| `MATRIX_RECOVERY_KEY` | – | Needed to sign in to an account that already has cross-signing and key backup |
| `ZEN_URL` / `ZEN_TOKEN` | `http://127.0.0.1:$ZEN_PORT` (port from `~/.zenbot/env`, else 8100) / `~/.zenbot/token` | The kernel it talks to |

---

## `~/.zenbot/` (config and state on the host)

| Path | Written by | Read by | Notes |
|---|---|---|---|
| `env` | `install.sh` | systemd (`EnvironmentFile`), `zen` CLI, `zen-matrix` (`ZEN_PORT`), scripts (`lib.sh::zen_env`) | Service environment; mode 600; hidden from sandboxed shells |
| `token` | `lib.sh::new_token` (install, dev) | `zen` CLI, `install.sh` (copies into `env`), `secrets.rs`, `zen-matrix`, scripts | **Secret.** API token; hidden from sandboxed shells |
| `version` | `install.sh`, `apply-upgrade.sh` | `update.rs`, `zen` banner | Installed commit |
| `upgrade.log` | `apply-upgrade.sh`, `update-engines.sh` | owner, `update.rs` (last line), `zen upgrade` | Upgrade and engine-update results |
| `upgrades/` | `upgrade.sh` (one stage per request: the tested `zend`, `zen`, `zen-engine`, `commit`, `note`; stages over 2 hours old removed), `apply-upgrade.sh` (`.lock`, `.installed`; removes its stage) | `apply-upgrade.sh` | Builds waiting to install; the newest request wins |
| `engines.json` | `update-engines.sh` | `zen status` | Engine versions from the last check |
| `engines.lock` | `update-engines.sh` | `update-engines.sh` | `flock` so only one engine update runs at a time |
| `history` | `zen` TUI | `zen` TUI | Prompt history (JSON lines, mode 600) |
| `backups/` | `db.sh backup` (before pending migrations; last 10 dumps kept) | owner | Database dumps |
| `backups/prompt-files/` | `memory::backup_file`: before the sleep promotes into or compacts a prompt file, and before an agent's `edit`/`write` to one (`dispatch`) | owner | Copies of `SOUL`/`IDENTITY`/`AGENTS`/`USER` (D-045); the sleep removes those older than 90 days |
| `bin/` | `install.sh`, `apply-upgrade.sh`; `scripts/matrix.sh` (`zen-matrix`) | systemd, `~/.local/bin/zen` link | `zend`, `zen`, `zen-engine` (+ `.prev` for rollback), `zen-matrix` |
| `outputs/` | `tools.rs`, `web.rs`, `mcp.rs` | the model (`read`, `bash`); the nightly sleep removes these files after 30 days | Full output of cut tool results; PDFs from `web_fetch` (`web-*.pdf`); MCP output over 50 KB (`mcp-*.txt`). All three use `outputs_dir()` (`<zen home>/outputs`, mode 700) |
| `engine/claude/`, `engine/complete/`, `engine/codex/`, `engine/sockets/` | `zen-engine` (`turn::engine_dir`, mode 700) | Claude Code, Codex | Fixed directories for Claude Code sessions and completions, Codex's empty working dir and private `CODEX_HOME`, per-turn tool sockets |
| `AGENTS.md` | `defaults.rs` when missing, then the owner | `compile.rs` | System-wide prompt file: the environment |
| `USER.md` | `defaults.rs` when missing, then the owner, the agent and the sleep (`## Learned`; backed up on every change) | `compile.rs` | System-wide prompt file: the owner (D-045) |
| `agents/zenbot/SOUL.md` | `defaults.rs` when missing, then the owner | `compile.rs` | The agent's own prompt file: who it is (`layout::SOUL`; one agent today) |
| `agents/zenbot/IDENTITY.md` | `defaults.rs` when missing, then the agent and the sleep (backed up on every change) | `compile.rs` | The agent's character and how it works, after the soul (D-045); `edit`/`write` on it (and SOUL, AGENTS, USER) refused for subagents and tainted sessions (`agent::prompt_file_refusal`) |
| `global/MEMORY.md` | `memory::export` | the owner | Copy of short-term memory (edit with `remember`, not here) |
| `global/skills/<domain>/<name>/` | `defaults.rs` (`work/brief`, `work/verify`, `work/close`) when missing, then `save_skill` (and the agent's file tools), the sleep (archiving) | `skills.rs`, `workshop.rs` | Skills; a git repository once the workshop first commits. `_proposed/<domain>/<name>`: drafts; `_archived/…`: rejected or unused skills |
| `global/tools/<name>/` | `save_tool` | `workshop.rs` (catalog, `run_made`) | The agent's own tools: `tool.json` and files; a git repository (`ZEN_TOOLS_DIR`). Approval lives in `made_tools`, not here |
| `global/wiki/` | `wiki.rs` (`capture`: pages, `index.md`, `log.md`, git commits), the agent (`edit` of summaries), the sleep (commits) | `search.rs` (indexer), the model (`read`), the owner | The wiki (`ZEN_WIKI_DIR`), its own git repository |
| `mcp.json` | the owner | `mcp.rs` (re-read when it changes) | MCP servers (`mcpServers`); keep secrets in `env` and refer to them as `${VAR}` |
| `SOUL.md`, `MEMORY.md`, `wiki`, `skills`, `tools` (symlinks) | `layout::migrate` | a rolled-back build | The old flat layout's paths, each a relative symlink to its new place (D-040); removed in a later release |
| `evals/<run>/` | `eval.sh` | `eval-report.sh` | Eval results |
| `matrix.env` | the owner | `zen-matrix` | **Secret** (mode 600): `MATRIX_USER`, `MATRIX_OWNER`, `MATRIX_PASSWORD` (first sign-in only), optional `MATRIX_RECOVERY_KEY`, `MATRIX_HOMESERVER` |
| `matrix/` | `zen-matrix` | `zen-matrix` | Mode 700. **Secrets:** `session.json` (access token), `store.key` (encrypts `store/`, the SQLite key and sync store), `recovery-key`. `state.json`: rooms ↔ sessions, job-report cursor |

Claude Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`.

---

## Scripts, install, deploy, CI

| Path | Does | Touches |
|---|---|---|
| `install.sh` | Fresh-VM install (safe to re-run): apt packages (git, curl, ca-certificates, jq, bubblewrap, poppler-utils, docker, docker compose), Node 22 in `~/.local/node`, Claude Code and Codex CLIs, binaries (download or build), `~/.zenbot/{bin,env,token,version}`, the `zenbot` systemd unit (and `remove_old_timers`), git hooks path. Doesn't install `zen-matrix` (`scripts/matrix.sh`) | system, `~/.zenbot`, `/etc/systemd/system` |
| `scripts/upgrade.sh` | Build (or fetch) → `cargo test` (local builds) → ping `zen-engine` → one scripted `faux/smoke` turn on a second kernel (`:18199`, throwaway copy of the live DB, so migrations are tried there; its own `ZEN_HOME` in the smoke workspace; OpenRouter and search keys unset, `ZEN_JOBS=0`) → stage the tested binaries and commit in `~/.zenbot/upgrades/<id>` → schedule `apply-upgrade.sh <stage>` via `systemd-run`. `--check` stops before staging | `target/`, temp DB `zen_smoke_*`, `~/.zenbot/upgrades` |
| `scripts/apply-upgrade.sh` | Detached, with the stage `upgrade.sh` made: wait for `"busy":0` (up to 30 min, then goes ahead), lock and step aside if a newer request is waiting or was installed, back up DB if migrations are pending, swap in the staged binaries (keeps `.prev`), write the stage's commit to `version`, restart, health check, roll back if unhealthy; when healthy, `remove_old_timers` and `docker compose … up -d` (so old timers go and compose services such as SearXNG arrive with an upgrade). Swaps `zend`, `zen`, `zen-engine` only | `~/.zenbot/{bin,version,upgrade.log,backups,upgrades}`, service |
| `scripts/self-update.sh` | `git pull` main then `upgrade.sh`; refuses a dirty checkout or another branch. Used by `zen upgrade`, `/upgrade`, `POST /api/upgrade` | checkout |
| `scripts/fetch-release.sh` | Download CI's binaries for HEAD into `target/release` (checksum verified); fails (changing nothing) on local changes under `crates/`, `Cargo.*`, non-x86_64-Linux, or no build. `--check REF` only checks | `target/release` |
| `scripts/update-engines.sh` | Daily (the kernel's `engines` job): update Claude Code / Codex CLIs, test with a real `complete` through `zen-engine`, roll back on failure; `--check` reports only | CLI installs, `upgrade.log`, `engines.json` |
| `scripts/db.sh` | DB helpers run inside the Postgres container: `pending`, `backup`, copy/drop/restore helpers | live DB, `~/.zenbot/backups` |
| `scripts/lib.sh` | `zen_env`, `wait_healthy`, `ensure_rust`, `new_token`, `remove_old_timers` (stops and deletes the `zen-sleep`/`zen-engines` timers older versions installed; `install.sh` and, after a healthy upgrade, `apply-upgrade.sh`) | `/etc/systemd/system` |
| `scripts/dev.sh` | Dev kernel in the foreground on `:18100` with database `zen_dev` and `ZEN_HOME` `~/.zenbot-dev`, using `~/.zenbot/env` settings | `zen_dev` DB, `~/.zenbot-dev` |
| `scripts/e2e.sh` | End-to-end scenarios (below); scripts in `scripts/e2e/*.json`, plus test servers in Python: `slow_worker.py` (a worker serving `slow/summarizer`, a deliberately slow `complete`), `mcp_server.py` (an MCP server with `echo` and `add`, stdio or `--http PORT`), `searxng_stub.py` (answers `/search?format=json`), `systemone_stub.py` (validates the direct classifier call), `ws_prompt.py` (sends one WebSocket message as zen's terminal app does and prints the events) | temp DB, workspace and `HOME` |
| `scripts/eval.sh`, `scripts/eval-report.sh` | Harness eval: this checkout vs installed (or `--base REF`), same model; each kernel gets its own `ZEN_HOME` next to the task workspace (default prompt files and skills); report for the owner, never a gate | `zen_eval_*` DBs, `~/.zenbot/evals/` |
| `scripts/git-hooks/prepare-commit-msg` | Adds `Zen-Session` / `Co-Authored-By` trailers when `ZEN_SESSION_ID` is set | commit messages |
| `deploy/compose.yaml` | `postgres` (pgvector, `127.0.0.1:5432`) and `searxng` (pinned by digest, `127.0.0.1:8888`, keyless search for `web_search`) | Docker volume `zen-pg` |
| `deploy/searxng/settings.yml` | SearXNG settings mounted read-only: JSON output on, limiter and image proxy off, not a public instance | — |
| `scripts/matrix.sh` | Build and test `crates/zen-matrix`, install `~/.zenbot/bin/zen-matrix`, `zen-matrix login` if not signed in, install and restart `zen-matrix.service`. `--build` stops after the tests | `~/.zenbot/{bin,matrix}`, `/etc/systemd/system` |
| `scripts/check-docs.py` | The docs still match the code: the lists the docs keep (routes, tools, settings, tables, events, tape kinds, slash commands, e2e scenarios, crates) against the code, paths, settings and `D-NNN` named in current docs exist; with `--base <ref>`, a change to the code adds a PROGRESS.md entry. CI job `docs` | – |
| `scripts/matrix-e2e.sh` | The Matrix channel end to end: a throwaway continuwuity homeserver in Docker, a faux kernel (temp DB, own port and home), the bridge and the scripted owner (`examples/owner.rs`) through E2EE; also a stranger's invite declined and a job report delivered | Docker container, temp DB |
| `deploy/zen-matrix.service` | Runs `~/.zenbot/bin/zen-matrix run` once `~/.zenbot/matrix/session.json` exists; `ProtectSystem=strict`, writable only `~/.zenbot/matrix` | — |
| `deploy/zenbot.service` | Runs `~/.zenbot/bin/zend` with `EnvironmentFile=~/.zenbot/env`; `ExecStartPre` brings Postgres up (must succeed) and SearXNG (a failure is ignored); `Restart=always` | — |
| `.github/workflows/ci.yml` | On PRs and pushes to main: build, `cargo test`, clippy `-D warnings`, `scripts/e2e.sh`; job `docs` runs `scripts/check-docs.py`; job `matrix` builds, tests and lints `crates/zen-matrix`. On main, if green: build `dist` profile, publish `zenbot-x86_64-linux-<sha12>.tar.gz` to the rolling `edge` release (20 newest kept) | GitHub releases |

---

## Tests

- **Unit tests** live in `#[cfg(test)] mod tests` at the bottom of each file, in every crate
  (`zen`'s `tui/` render and key tests use `tui/test_util.rs`). Run `cargo test --release`; for
  `zen-matrix`, `scripts/matrix.sh --build` (its own workspace).
- **End to end:** `scripts/e2e.sh [filter]` builds, then runs each scenario on a fresh kernel (port
  18377, `ZEN_FAUX=1`, `ZEN_WORKERS=engine`) with its own git workspace, all on one throwaway
  database (`zen_e2e_<pid>`), checking the database. At the end it checks every tape is numbered
  without gaps and its hash chain recomputes. Scenarios (23): `open-loop` (also message paging by `last`/`after`), `restart-recovery` (an old session in a workflow state still
  works), `prompt-files` (defaults installed in the scoped layout, the owner's kept, all in the instructions, the tool
  list), `layout-move` (an old flat home moves once into `agents/` and `global/`, nothing overwritten,
  symlinks left at the old paths), `skills` (found and loaded on demand, nothing outside a skill), `wiki` (`capture` makes a
  page then appends to it newest first, masks a secret, commits, keeps `index.md` and `log.md`;
  `search` finds the page; a capture after `web_search` is labelled `web`; the sleep reports a page
  with no summary), `workshop` (a new skill is a draft, a near-duplicate and a missing reason are
  refused, a new domain waits for the owner; drafts found and loaded; a made tool found and run
  read-only; accepting the session activates the draft; `zen skills accept` activates the new
  domain's; after `zen tools accept` the tool can write; both folders in git; `zen skills` lists
  use), `delegation` (two subagents, one on a named model and one by the owner's policy, report
  back; they get no `ask` or `delegate`; choices logged with their probability; the parent's
  verdict shows in `zen policy`; `zen policy undo`; `ask` with `wait: false` keeps working), `mcp` (find, load and call
  tools on a stdio and an HTTP test server, a missing argument refused, remote output wrapped and
  the session tainted), `web` (loopback and metadata addresses refused, search through a SearXNG
  stub, one envelope with markers defused, a tainted session's memory saved as `inferred`),
  `search` (a fact from one session found from another by words and by exact path first, a memory
  found and counted as used, `history` reads the other session, every search logged),
  `memory-across-sessions`,
  `memory-sleep` (the ceiling, a sleep tidies to size, archives, records, the morning note), `ask`
  (ends the turn; `move` is gone), `verifier` (commands by the kernel, a read-only verifier,
  a failed command skips it), `summaries`, `secrets`, `slow-summary` (a
  summary at the hard limit slower than the watchdog: the turn waits, then runs), `system-one`
  (the direct System One call through `systemone_stub.py`: `decide` answers parsed and logged; a
  the sleep promotes into `USER.md` under `## Learned` with a
  backup and copies knowledge into the wiki; an agent's over-cap edit of `USER.md` is backed up and
  the sleep compacts it through a kernel session), `kernel-tools` (the zen home's files aren't
  attached as project context, edit keeps one trailing newline, a read over 16 MiB is refused), `stale-turn` (a
  turn the kernel ended keeps running in the worker; its late answer must not reach the next turn),
  `scheduled-jobs` (`jobs.json` and the System One stub, `ZEN_JOBS=1`: system jobs seeded; an
  owner's job runs once in a `job` session with only its context, can't schedule, report recorded,
  next run an hour on; a silent run archived; the fifth failure pauses; a run past its grace
  missed, one within it caught up; a second run refused; a restart marks a running run
  interrupted; the agent's job paused below `ZEN_JOB_BAR` and live above it, the owner's resume
  approves; the `sleep` system job runs through the engine; `zen status` shows jobs),
  `names-suggestions` (the session is named after its first turn, a next prompt suggested each turn,
  outcomes `accepted`/`edited`/`declined`/`unseen` recorded, the owner's title never overwritten,
  `GET /api/suggestions` counts by prompt version). `secrets` also checks that the kernel's secret
  variables don't reach the agent's shell and that a `?token=` in the URL is refused (only the header counts).
  Faux scripts in `scripts/e2e/*.json`, named after what they drive (`reach.json` drives `mcp` and `web`; `wiki` also starts the SearXNG stub); `slow-summary` adds the
  `slow` worker from `scripts/e2e/slow_worker.py` via `ZEN_WORKER_SLOW_CMD`; `mcp` starts
  `scripts/e2e/mcp_server.py` (stdio, and `--http` on port +1), `web` starts
  `scripts/e2e/searxng_stub.py` on port +2 (`ZEN_SEARXNG_URL`), `system-one` starts
  `scripts/e2e/systemone_stub.py` on port +3 (`ZEN_S1_URL`). Nothing reaches the internet. Add a scenario when kernel behavior changes.
- **Evals:** `evals/tasks/<name>/{task.json,files/}` (14 tasks, including the runner self-test
  `smoke`), run by `scripts/eval.sh`; see `evals/README.md`. Harness changes (system prompt,
  history, tools, workers, model or effort handling) get an eval before commit; the report goes to
  the owner, who decides.
- **Upgrade smoke test:** one scripted `faux/smoke` turn inside `scripts/upgrade.sh`.
- **Matrix channel:** `scripts/matrix-e2e.sh` end to end (Scripts table); not run by `scripts/e2e.sh` or CI.

---

## Notes and known gaps

- **Columns and tables of the old workflow stay** (expand-only): `sessions.state` and old tape
  blocks. Nothing writes `state` any more and the API no longer returns it; `compile::history`
  still honours an old `state` block with `fresh: true` (history starts after it), so old sessions
  replay as before. Drop them in a later release.
- **Criteria commands run unsandboxed.** `verify` runs a criterion's `run` command on the bash
  tool's core (`tools::run_shell`: own process group, 600 s timeout, output masked) with
  `read_only = false`, so outside bubblewrap and able to write, even though the verifier's own
  shell is read-only.
- **Workers start through a login shell** (`bash -lc` in `mind.rs`), so the service user's
  profile can change their environment.
- **`GET /api/mcp` connects every configured server** (starts stdio ones) to count their tools.
  No client calls it (not `zen status`).
- **The skill rules apply only through `save_skill`.** The agent's `write`/`edit` can change any
  skill or tool file directly (including activating a draft by moving it); the next workshop commit
  records it. Changes to an active skill through `save_skill` apply at once, without review.
- **The duplicate check covers only new skills in the same domain**, against active skills and
  drafts; System One's judgment is used only with private content allowed, else word overlap.
- **`bash` can still write the prompt files.** `prompt_file_refusal` covers `edit`/`write` only; a
  shell command runs as the owner's Unix user (ROADMAP.md, debt).
- **Agent edits to the wiki are committed only by the next capture or sleep**, together, as one
  commit; git failures (e.g. no `git`) are ignored silently.
- **`zen policy undo` toggles.** It saves a copy of the version before the latest, so a second
  undo restores what the first undid rather than going further back.
- The protocol as built is in `docs/worker-protocol.md`; any sketch of it in `SPEC.md` is a target,
  not the code.
- **MCP stdio servers** start with a clean environment (PATH, HOME, USER, LANG, LC_ALL, TZ, TMPDIR and
  the config's `env`), so the kernel's token and keys never reach them; they still run outside
  bubblewrap, and remote MCP URLs aren't address-checked like `web_fetch` (the owner configures them).
- **Search and private content:** embeddings (and System One reranking) send session and memory
  text to OpenRouter only while `ZEN_S1_PRIVATE` allows private content (D-032); results recalled
  from a session that read web content (by `search` or `history`) are wrapped as untrusted and taint
  the recalling session. A changed `ZEN_EMBED_MODEL` re-embeds in the background; vector search
  compares only rows of the current model. The indexer takes tape events once they are 5 seconds
  old, so an id committed late isn't skipped.
- **Wiki and untrusted content:** search results from a wiki page holding entries labelled `web`
  are wrapped as untrusted and taint the session; a page read directly with `read` isn't. The wiki
  indexer compares file times in milliseconds.
- **Made-tool approval covers the content** (a SHA-256 of the manifest and files, computed in
  Postgres): a tool changed after approval runs sandboxed again. Unapproved tools are offline and
  read-only, with an empty home, but can read the rest of the filesystem (`--ro-bind / /`); approved
  ones run with `bash -c` (not a login shell) and full write access.

## Deeper docs

| Doc | What's there |
|---|---|
| `AGENTS.md` | How to change zenbot: build, test, upgrade, conventions, rules. Read it fully first |
| `CONTEXT.md` | What zenbot is for, how success is measured, constraints. Read first |
| `DESIGN.md` | How the system works today, then the target design agreed on 2026-10-06 |
| `DEVELOPMENT.md` | Local loop: build, test, verify and ship a change |
| `ROADMAP.md` | Phases in order |
| `PROGRESS.md` | Append-only log of what shipped and what was learned |
| `DECISIONS.md` | Why things are the way they are, newest first (D-001 to D-049; D-025 onward: the target design and the choices made building it) |
| `SPEC.md` | Long-term target modules (section numbers kept, e.g. 5.18 Security) |
| `docs/context.md` | How each turn's context is built, stored, cached, summarized and measured; the tape |
| `docs/brief.md` | Briefs and verification: the `work/brief` and `work/verify` skills, the `verify` and `ask` tools |
| `docs/worker-protocol.md` | Kernel ⇄ worker JSON-RPC: methods, messages, engine sessions, guarantees |
| `docs/client-protocol.md` | HTTP API and WebSocket events for clients |
| `docs/matrix.md` | The Matrix channel: what it does, security, setup, its code, limits |
| `docs/research/*.md` | Dated research snapshots (2026-10-06), kept as history |
| `evals/README.md` | Eval tasks and the report |
| `README.md`, `INSTALL.md` | Using and installing zenbot |
