# zenbot — Codebase Map

> **Purpose:** a blast-radius guide for coding agents. Before touching anything, find it here to see
> what it does, what it depends on and what depends on it. The map is not the territory: read the
> code before changing it, and if the two disagree, the code wins (then fix the map).
>
> Written from the code on 2026-10-06; audited against the code on 2026-10-10 at `6217f27` (every
> file, route, table, setting and script checked). Where something couldn't be confirmed in the
> code it says "unverified". `wc -l` shows which files are big.

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

- **The invariants** (the kernel owns all state and runs every tool call, the tape is the truth,
  the prefix is fixed per session, …) are in DESIGN.md ("Invariants"); how the engines' own tools
  are kept off and checked at run time, in the `zen-engine` table below.
- **One process tree.** systemd runs `zend` (`deploy/zenbot.service`); `zend` starts the workers
  as children (`bash -c <cmd>`) and restarts them when they exit (`workers.rs::supervise`).
- **Never restart `zend` yourself** (it kills the session you run in). Use `scripts/upgrade.sh`.

---

## Layer 1 — Kernel `zend` (`crates/zend`)

Single binary (axum, sqlx, tokio). Modules are declared in `main.rs`; most share `App` through
`use super::*`. Besides Rust: migrations, default prompt files and skills, and the verifier's prompt.

### Files

| File | Role | Main items | Depends on | Used by |
|---|---|---|---|---|
| `src/main.rs` | Startup and shared state. Reads the boot settings (and logs settings `config.rs` couldn't parse), writes missing default prompt files and skills (`defaults::install`), connects to Postgres, runs migrations (`set_ignore_missing(true)` so a rolled-back build starts on a newer schema), repairs the tape, spawns workers, starts background tasks (dispatch, watchdog, `score::idle_loop`, `search::index_loop`, `jobs::run_loop`, the updater), builds the router (listens on `ZEN_BIND`, localhost by default, D-041) | `App` (incl. `hubs`, and `background`: kernel work outside turns, counted as busy through `Background` guards), `zen_home` (`ZEN_HOME`, else `~/.zenbot`), `subscribe`/`unsubscribe`/`emit` (a session's event hub exists only while a client is connected; `emit` with no client sends nothing), `valid_name` (the one name rule for skills, domains, made tools and jobs: lowercase letters, digits and single hyphens, not at either end; 64 characters, 40 for a job), `outputs_dir`, `load_messages`, `context_in` | every module | everything (via `App`) |
| `src/assist.rs` | Small model calls around the owner's turns (D-047): after an owner's turn ends cleanly (not in jobs or child sessions), one call to the assist model (`ZEN_ASSIST_MODEL`, Haiku 5.5) names the session while `title_source` isn't `owner` (first 3 turns) and suggests the next prompt (not after `ask`; `ZEN_SUGGEST=off` stops it); emits `title` and `suggestion`; `settle` records what the owner did with it when the next prompt arrives | `after_turn`, `settle`, `stats` (`GET /api/suggestions`), `PROMPT_VERSION`, `model` | `workers::complete`, `tape` | `turns.rs` (`finish_turn`), `api.rs` (`prompt`) |
| `src/api.rs` | HTTP handlers and the WebSocket (docs/client-protocol.md). Token auth middleware (header `Authorization: Bearer` only, compared in constant time; a token in the URL is refused). A WebSocket `prompt` starts its turn in a separate task, so an `abort` is read while the turn is prepared. `run_sleep` runs the sleep in its own task, so a client that stops waiting doesn't cut it short | `auth`, `same_secret`, `health`, `version`, `upgrade_*`, `list_models`, `list_sessions`, `board`, `*_session`, `decide`, `DECISIONS`, `list_memory`, `list_skills`, `review_skill`, `review_tool`, `get_policy`, `put_policy`, `undo_policy`, `run_sleep`, `mcp_status`, `list_jobs`, `create_job`, `update_job`, `delete_job`, `run_job`, `list_job_runs`, `handle_socket` | `turns` (start/abort), `memory`, `workshop`, `delegate`, `jobs`, `assist` (`settle`), `mcp`, `score`, `workers`, `update` | router in `main.rs` |
| `src/turns.rs` | A turn's lifecycle: start (owner or kernel), compile what it sends (tools and instructions from `agent::specs` / `agent::system_for` by the session's kind), record it in `turns`, finish, abort, watchdog, child sessions (`run_child`: a verifier or a subagent, optionally on another model, inheriting the parent's taint; returns the child's id, stops it after 30 minutes, or as soon as whoever waits for it stops: `StopOnDrop`). `Turn.kind` is read once at start. A summary made inline at the hard limit is `hold`-counted as a running tool so the watchdog waits, and an abort stops it. `record_turn` returns the row plus the session's parent and cost in one query; `per_turn_report` makes a resumed engine session's totals per turn, and adds up a failover turn's `provider_reports`. `turn.abort` always names the turn, and is sent again if the turn was aborted while `turn.start` was on its way | `Turn`, `Origin`, `Recorded`, `start_turn`, `run_kernel_session` (a parentless kernel session: the sleep compacting a prompt file), `run_and_wait`, `begin_turn`, `still_ours`, `hold`, `finish_turn`, `abort_turn`, `run_child`, `watchdog`, `record_turn`, `session_cost` | `agent`, `compile`, `compact`, `measure`, `tape`, `workers`, `assist` (`after_turn`) | `api`, `dispatch`, `agent`, `workers`, `delegate`, `memory`, `jobs` |
| `src/dispatch.rs` | Handles every message from workers, read as `zen_proto::worker`'s `ToolCall` and `TurnEvent` (a misshapen or unknown one is logged and refused; a misshapen `turn.end` still ends its turn) (notifications in order per session, one queue task per session; tool calls concurrently): `tool.call` (refused after a tool ended the turn, `agent::refuse`; a verifier may run only `bash`, `read`, `submit_verdict`; then `agent::run_tool` or `tools::execute`), `turn.delta`/`turn.thinking`, `turn.message` (tape + `model_calls`), `turn.fallback` (a `failover` tape block, a `status` event, and the session moved to the target model: `sessions.model`, effort kept only if the model takes it (`workers::effort_on`, shared with `PATCH /api/sessions/{id}`), the turn's model, and a `model` event; D-058), `turn.usage`, `turn.end`. Before a tool runs: `agent::refusal` by kind, `agent::protected_refusal`, `workshop::mark_draft_write` (a tainted session's `edit`/`write` into a draft skill marks it first); effects pass `gate::check` first (run / sandbox / hold; a verifier's shell remains read-only); a `read` of stored tainted content (a wiki page with `web` entries, a marked draft: `agent::read_of_tainted`) is handed over through `taint::hand_over_rating`; a prompt file is backed up (`memory::backup_file`) before an `edit`/`write` to it and `agent::over_cap_note` added when it ends over its cap. Drops messages not from the turn running on that worker (matched by echoed `turn_id`; by session and worker when a message has none). Attaches newly met AGENTS.md files to tool results: as instructions from a trusted repository (`context::trusted`), else wrapped as untrusted after tainting the session (`context` block with `trusted`). Tool arguments are masked and cut at 4 KB per string before `tool_start` and `tool_calls` (`shown_args`). A tool call is counted in its turn's `tools_running` by a guard (`ToolCounted`) that uncounts it even if the call panics; a panicking tool still gets an error answer | `dispatch`, `handle_incoming`, `touch`, `turn_id`, `session_id` | `agent`, `tools`, `compact`, `context`, `taint`, `turns`, `memory` (`backup_file`) | `main.rs` (spawned task) |
| `src/agent.rs` | The agent's tools beyond files and the shell, in a fixed order after them (see "Tools the model gets"); runs `ask` (questions block; ends the turn unless `wait: false`), `verify` (kernel runs criteria commands, a child verifier session judges the rest; `verification` block), `decide` (when a System One model is set; logged as `point = 'tool'`) and the verifier's `submit_verdict`; hands `history`, `search`, `remember`, `capture`, `web_*`, `find_skills`/`load_skill`, `save_skill`/`save_tool`, `find_tools`/`load_tool`/`call_tool`, `delegate` and `schedule` to their modules. Tools per session kind are allow-listed (`refusal`): a subagent (also the sleep's kernel sessions) or job session gets no `ask`, `delegate` or `schedule`, a verifier only `bash`, `read`, `submit_verdict`, an unknown kind nothing; a subagent gets `delegate::system_for` instructions. `verify` refuses criteria with duplicate ids. `protected_target` / `protected_refusal` (symlinks, `..` and missing folders resolved): `edit`/`write` on SOUL, IDENTITY, AGENTS or USER, an active or archived skill or a made tool refused for any session with a kind (subagents, verifiers, jobs, kernel sessions) and tainted sessions (D-045, D-051); on `mcp.json`, `env`, `env.prev`, `token`, `db-password`, `matrix.env`, `matrix/` or `bin/` refused for every session; `over_cap_note` asks the agent to compact a file its edit took over its cap. `draft_target` names the draft skill an `edit`/`write`/`read` is about; `read_of_tainted` tells a `read` of stored tainted content. `history` of a tainted session and `find_skills`/`load_skill` showing a marked draft go through `taint::hand_over_rating`; `verify`'s commands run in the session's shell sandbox. Replaced `flow.rs`: no workflow (docs/brief.md) | `specs`, `system_for`, `read_only`, `run_tool`, `refuse`, `refusal`, `Protected`, `Guarded`, `protected_target`, `prompt_file_target`, `protected_refusal`, `draft_target`, `read_of_tainted`, `over_cap_note`, `log_decision`, `combine`, `render_results`, `VERIFIER` | `tools` (`run_shell` for checks), `memory`, `search`, `wiki`, `skills`, `workshop`, `delegate`, `jobs`, `web`, `mcp`, `compact`, `compile` (`file_cap`), `git`, `secrets`, `score`, `turns` (`run_child`); `steps/verify.md` | `dispatch`, `turns` |
| `src/memory.rs` | Short-term memory and the sleep (DESIGN.md "Memory and skills"): the `remember` tool (what a tainted session saves or replaces counts as `inferred` and is marked `tainted`), rendering only original risk 0–1 into instructions after recording writer dependencies and rechecking contamination (other entries by id only, `left_out`), export to `~/.zenbot/global/MEMORY.md`, the hard ceiling (refuse + start a sleep), `sleep` (System One judges up to 4 entries at once; `covered` drops entries something else already says, keeping one of any that say the same; promote lasting entries: `belongs_in`, `promote_to_file` into `USER.md`/`IDENTITY.md` under `## Learned` (skipped when the line is already there; never a marked entry, `may_steer`), written once the entries are locked, `wiki::capture_memory` (labelled `web` for a marked entry); a marked entry never covers a clean one, and the note never shows its text (`shown`); `compact_file` for a prompt file over its cap through `turns::run_kernel_session` (a `subagent`-kind session with no parent), `compacted_text` checks the answer; `backup_file` into `backups/prompt-files/` (also before an agent's edit, from `dispatch`; the `prune` job removes them at 90 days); then rank, keep, archive in one transaction, leaving an entry changed since the sleep read it as it is; one at a time), the morning note (`morning_note`). The sleep tidies memory only: the wiki, skills, routes and pruning are system jobs of their own (`jobs.rs`). | `cap`, `render`, `export`, `spec`, `run_tool`, `sleep`, `belongs_in`, `with_learned`, `promote_to_file`, `plan`, `covered`, `Cover`, `priority`, `questions`, `morning_note`, `last_run`, `list` | `score` (`decide`, `private_ok`), `secrets`, `taint::limited`, `wiki::capture_memory`, `turns::run_kernel_session` | `agent`, `compile`, `api`, `dispatch` (`backup_file`), `jobs` (the `sleep` system job, `BACKUPS` for `prune`) |
| `src/skills.rs` | Skills in `~/.zenbot/global/skills/<domain>/<name>/` (`ZEN_SKILLS_DIR`): frontmatter, validation (agentskills.io rules; a description can't contain `<` or `>`), scan (folders starting with `.` or `_` aren't domains), the index for the instructions (active skills only), `find_skills` word matching, `load_skill` (SKILL.md or a file inside the skill, cut at 48 KB). Drafts (`_proposed/<domain>/<name>`, `drafts()`) are found and loaded too, their description prefixed `(draft)`, or `(draft, untrusted)` for one in `workshop::tainted_drafts`; `run_tool` says when its result shows such a draft, and `agent` hands it over as untrusted | `root`, `frontmatter`, `validate`, `scan`, `drafts`, `index_text`, `find`, `lookup`, `load`, `find_spec`, `load_spec`, `run_tool` | — | `agent`, `compile`, `workshop` |
| `src/defaults.rs` | Writes the default prompt files and skills (`defaults/`, compiled in) that are missing under `~/.zenbot`; never overwrites; a skill only when its whole folder is missing | `install` | — | `main.rs` |
| `src/search.rs` | Search over past sessions, memories and the wiki. `index_loop` (every `ZEN_INDEX_SECS`, sooner while there's a backlog) keeps `search_docs` current: one document per turn of every non-verifier session (the owner's message, the answers and tool calls, not tool output, which isn't read; a kernel prompt continues its turn; tape events taken once 5 seconds old, watermark `tape` in `search_state`; unchanged documents aren't rewritten), one per short-term memory and old `long` row (from `search_queue`, which a trigger fills in the change's own transaction; archived and deleted ones removed), one per wiki page (`kind = 'wiki'`, `ref` = the page's slug; by each page's file time against the time it was indexed, reading only changed pages; deleted pages removed); then, outside the indexing lock, embeddings through an OpenAI-compatible `/embeddings` (`ZEN_EMBED_URL`, default OpenRouter; `ZEN_EMBED_MODEL`, 1536 dimensions; embeddings of another model cleared once per start and made again; a turn once its session has no turn running or after 2 minutes unchanged; a batch the provider refuses for its input halved until the refused row is alone, which is tried 3 times, `embed_attempts`/`embed_error`) when `OPENROUTER_API_KEY` is set, `ZEN_EMBED` isn't `0` and `ZEN_S1_PRIVATE` allows it. A query: exact names and paths first (trigram over `ident`), then full text and vectors of the current model merged by reciprocal rank fusion, each arm filtering `search_docs` itself so it uses its index (HNSW with `hnsw.iterative_scan = strict_order`); System One reranks (`search_rerank` decision, only with `private_ok`). The `search` tool (scope `all` = sessions, memories and wiki; `sessions`, `memories`, `wiki`, `this_session`) indexes first, marks returned memories as used, logs every search in `searches`, and checks each result's mark (a turn of a tainted session, a memory marked `tainted`, a wiki page with an entry labelled `web`; unreadable counts as marked): clean results are shown, a marked exact match is handed over through `taint::hand_over_rating` (wrapped as untrusted, the session tainted), any other marked result is listed by reference only, without its text (`render_withheld`, D-056) | `index_loop`, `index_once`, `refresh_wiki`, `identifiers`, `turn_text`, `split_turns`, `exact_candidate`, `query`, `Found`, `spec`, `run_tool` | `score` (`decide`, `private_ok`), `agent::log_decision`, `wiki` (`root`, `page_times`, `documents_where`), `taint` (`hand_over_rating`) | `agent`, `wiki` (`index_once`, `query` for candidates), `main.rs` |
| `src/wiki.rs` | The wiki: markdown pages in `ZEN_WIKI_DIR` (default `<zen home>/global/wiki`), a git repository (`ensure` runs `git init`, adds `index.md` and `log.md`). A page: frontmatter (type: concept, entity, decision, playbook, project, person; aliases), a title, a summary above `---`, then a dated timeline, newest first. The `capture` tool: masks secrets in the note, gathers candidate pages (exact title or alias, then `search` over `wiki`), System One picks a page or `new` and judges `known` and `sensitive` (`capture` decision; only with `private_ok`; without it an exact title match or a new page), skips a known note (≥ 0.8), refuses a sensitive one (≥ 0.8), appends `- **date** \| from, session id (source) — note` (source `web` when the session is tainted, or for the sleep's copy of a marked memory), rewrites `index.md`, appends to `log.md`, commits as `zenbot`. The agent rewrites summaries itself with `edit`. `lint`: pages with entries and no summary, links to missing pages (a page with `web` entries isn't named: the note goes into instructions). `has_web_entry` tells a page with `web` entries | `root`, `slug`, `Page`, `new_page`, `parse`, `add_entry`, `index_text`, `lint`, `tend` (the nightly `wiki` job: commit the agent's edits, lint lines), `ensure`, `capture_memory` (the sleep copies lasting knowledge into the wiki), `has_web_entry`, `spec`, `run_tool`, `page_times`, `documents_where` | `search` (`index_once`, `query`), `score` (`decide`, `private_ok`), `agent::log_decision`, `secrets`, `taint::limited`, `git` (`run`, `commit_all`) | `agent`, `search` (indexer), `memory` (sleep), `jobs` (the `wiki` job) |
| `src/workshop.rs` | The agent improves its skills and makes tools. `save_skill` (domain, name, description, body, optional `files`, a required `reason`): name rules, SKILL.md under 10,000 characters, validated in a scratch copy first; updating an active skill applies at once, an existing draft is updated; a new skill is first checked against the same domain's skills (System One `skill_duplicate` decision, refused at ≥ 0.8; without System One or private content, word overlap ≥ 0.6) and saved as a draft in `skills/_proposed`. A draft becomes active when the owner accepts it (`decide_draft`) or when the owner's `accept` verdict on a session that loaded it arrives (`on_accept`, from `api::decide`); a draft in a new domain needs the owner. A draft a tainted session writes (`save_skill`, or `edit`/`write` into it: `mark_draft_write`) is marked in `draft_taint` first and its commit says so; `on_accept` never activates a marked draft, and the owner's `decide_draft` clears the mark (`tainted_drafts` reads the marks, every draft when they can't be read). Every change is a commit in the skills folder (made a git repo on first commit). `tend` (the nightly `skills` job): active skills unused 30 days (since the last load or the SKILL.md change) are flagged in the note, at 90 moved to `skills/_archived`. `save_tool`: `tool.json` (name, description, parameters, command) and its files in `ZEN_TOOLS_DIR`/`<name>`, a `made_tools` row, a commit in the tools folder. `run_made` (from `mcp::call_tool` as `made_<name>`): runs a private copy of the tool's folder (a temp folder, dropped after the run: the copy is what is fingerprinted, and what the tool writes never reaches its folder), JSON arguments on stdin, a clean environment (PATH, HOME, USER, LANG, LC_ALL, TZ), 120 s limit, output cut at 50,000 characters and masked; unapproved, or changed since approval (its SHA-256 over manifest and files, computed in Postgres, differs from `approved_sha`): `bwrap` with `/` read-only, empty home, no network; approved and unchanged: `bash -c` unsandboxed only at session risk 0–1 or with an exact owner grant; rejected: refused. Its exact private snapshot is rated before spawn, with writer dependencies; binary/link or metadata failures refuse execution. Denial and new dependency edges serialize against the spawn boundary. `decide_tool` and the owner's `decide_draft` need the fingerprint the owner reviewed (8 characters or more of it) and refuse when the content changed. `stats` (loads, last load, accepted sessions per skill, each draft's fingerprint and `untrusted` mark; the tools with theirs) | `save_skill_spec`, `save_tool_spec`, `run_tool`, `decide_draft`, `on_accept`, `tainted_drafts`, `mark_draft_write`, `stats`, `tend`, `made_tools`, `made_entries`, `decide_tool`, `run_made`, `overlap`, `tools_root` | `skills`, `score` (`decide`, `private_ok`), `agent::log_decision`, `secrets`, `git::commit_all`, bubblewrap | `agent`, `api`, `mcp`, `jobs` (the `skills` job) |
| `src/delegate.rs` | Delegation and model choice. `delegate` takes a `task` or up to 8 `tasks` (each with optional `model`, `dir`), run at once (`join_all`); each is a child session of kind `subagent` via `turns::run_child` (1,900 s limit), and the parent gets each one's final answer with its model, kind, time and cost (wrapped as untrusted, tainting the parent, when the subagent is tainted). A subagent can't delegate (refused) or ask (not offered). Model choice when none is named: System One classifies the kind of work (`kind_of`: understand, shape, bet, build, verify, maintain, reflect, reach; `unknown` without System One or private content), `pick` follows the policy's route for that kind (else `default`, else the parent's model; only served models) and explores a candidate with probability `explore` (policy, else `ZEN_EXPLORE`, capped at 0.5). Each choice is a `model` decision (`session_id` = the child; input: kind, task, parent, how = asked/policy/explore; its probability). `policy` / `set_policy` read and version the `policies` table; `stats` (per kind and model: subtasks, judged and accepted by the owner's verdict on the parent, mean cost); `suggest` / `tune` (the nightly `routes` job) switch a route only when another model's Wilson lower bound beats the current one's upper bound (`wilson_lower`), each with at least `ZEN_POLICY_MIN_JUDGED` judged subtasks; the old model stays a candidate | `SUBAGENT`, `system_for`, `policy`, `set_policy`, `pick`, `final_answer`, `spec`, `run_tool`, `stats`, `suggest`, `tune` | `turns::run_child`, `workers` (`collect_models`, routes), `score` (`decide`, `questions`, `private_ok`), `agent::log_decision`, `taint` (`tainted`, `taint`, `untrusted`), `tape` | `agent`, `api`, `jobs` (the `routes` job), `memory` (`SUBAGENT`, `final_answer` for the sleep's compaction) |
| `src/jobs.rs` | Scheduled jobs (D-046): schedules (`parse_schedule`: cron via `croner` in the job's `chrono-tz` zone, `every`, `at`, `in`, checked arithmetic; `next_after`, `next_from` (an interval anchored on the due time), `period` (a cron's shortest gap over 50 runs), `grace`), the store (`create`, `update`, `set_enabled`, `remove`, `run_now`, `list`, `runs`), the gate for the agent's jobs (System One on the owner's own messages, `ZEN_JOB_BAR`; `job_approval` decisions), `seed` (system jobs `sleep`, `wiki`, `skills`, `routes`, `prune`, `engines`; an existing install gets a new one at start), `run_loop` (marks unfinished runs `interrupted`, sleeps until the earliest due job, `tick` claims with `FOR UPDATE SKIP LOCKED`, skips runs past their grace as `missed`, inserts a run `ON CONFLICT DO NOTHING` so a running one skips the job), runs (`run_system`: `memory::sleep`, `wiki::tend`, `workshop::tend`, `delegate::tune`, `prune_files` (saved outputs after 30 days, prompt-file backups after 90, database dumps but the newest 3 by the time stamp in their name, eval run directories named by a time stamp after 7 days; only names our scripts make) or `update-engines.sh` on `run_shell`; an upkeep run that found nothing is `silent`; `upkeep_note`: what the upkeep jobs found in the last day, for the morning note; `run_agent`: a session of kind `job` with a `base` block from `compile::compose` + skills + `<scheduled_job>` note, `turns::run_and_wait`; report, `[SILENT]`, `[FAILED]`, or the job's report file (D-055: `checked_report_file` when set, `read_report` after an `ok` run, fail closed); `post_target` (D-060: the owner's session a job posts into) and `deliver_pending` (durably queues an origin 0–1 `ok` report, then atomically records its writer edge and appends it once the target is idle and unstopped, serialized against denial and dependency insertion); the last report through `taint::hand_over_rating`, so a marked one is untrusted and taints the run), `finish` (marks `job_runs.tainted` when the run's session read untrusted content; backoff, pause after 5 failures), the `schedule` tool (`runs` hands marked reports over as untrusted) | `parse_schedule`, `next_after`, `next_from`, `period`, `grace`, `checked_report_file`, `read_report`, `create`, `update`, `set_enabled`, `remove`, `run_now`, `list`, `runs`, `gate`, `seed`, `run_loop`, `tick`, `run_system`, `upkeep_note`, `prune_files`, `run_agent`, `finish`, `spec`, `run_tool`, `JOB` | `compile`, `skills`, `memory`, `wiki`, `workshop`, `delegate`, `turns`, `score`, `taint`, `tools`, `config` | `main.rs` (`run_loop`), `agent.rs` (tool), `api.rs` |
| `src/web.rs` | `web_search` and `web_fetch`. Fetch: http(s) to public addresses only (own DNS resolver drops non-public addresses; IP literals and each of at most 5 redirects checked; no proxy), 10 s connect / 30 s / 5 MB, HTML to markdown (`dom_smoothie`, `htmd` fallback; Latin-1/Windows-1252 pages decoded by their charset), 20,000 characters per call paged by `offset`, 15-minute / 32-entry cache (oldest evicted), PDFs saved to `~/.zenbot/outputs/web-*.pdf` for `pdftotext`, `focus` keeps the parts System One judges relevant (`web_focus` decision). Search: Brave or Tavily when a key is set (or `ZEN_SEARCH_PROVIDER`), else SearXNG, which also rescues one failed keyed call; System One reranks (`web_rerank` decision). Every result is wrapped in an `<untrusted>` envelope and marks the session risky (not tainted). Fetch provenance uses the final URL and optional owner-only URL adjustment; search takes max source risk. Redirect hops get SSRF and egress checks, with a 30 s total deadline | `is_public`, `check_url`, `readable`, `chunks`, `render_hits`, `search_spec`, `fetch_spec`, `run_tool` | `score` (`decide`, `private_ok`), `agent::log_decision`, `secrets`, `tape`, `taint` (`taint`, `untrusted`) | `agent`, `memory`, `wiki`, `search`, `mcp` |
| `src/sources.rs` | D-062 source ratings: strict HTTPS exact host/segment-prefix allowlist, most-specific match, final-URL rating; owner-message-only System One adjustment, cache by owner sequence/exact URL/base; source changes audited. Native DB and owner URL replay tests are ignored tests with usage in their comments. Owner API GET/POST `/api/sources`; CLI `zen sources add/rm/list`, chat `/sources`, Matrix `!sources` | `canonical`, `key`, `base`, `page`, `list`, `change`, `raise` | `score`, `tape`, `gate` | `web`, `dispatch`, `api`, `taint` |
| `src/gate.rs` | D-056 three-way effect check: safe fixed reads, offline/read-only programmable work (D-063), held outward/unknown/steering effects; a hold that only can't classify doesn't taint (`escalates`). D-062 egress: in a risky session `web_fetch` runs for allowlisted targets or a whole URL in authenticated owner messages or kernel-verified web results (`fetch_allowed`, `url_seen`), including redirect checks. `action_gates` binds workspace/tool/arguments; owner GET/POST `/api/sessions/{id}/allow`, one retry in ten minutes, audit committed with approval. Project code cannot change workspace files without an exact grant; sandboxed attempts offer approval without tainting. File effects use directory descriptors, not mutable symlink names. The `action-gate` e2e covers the routes, credentials, grants, nested verification and egress | `Policy`, `check`, `policy`, `shell`, `escalates`, `allow`, `waiting` | `taint`, `tools`, `tape` | `dispatch`, `api` |
| `src/gate/grammar.rs` | The gate's shell grammar: parts and words, listed read programs each with its own flags (`sed` print scripts, `find` without actions, `rg` without preprocessors), `cd`, globs expanded and checked here, `/dev/null` redirects; re-emits the command for the read-only view. the ignored test `gate_replay` replays recorded commands (usage in its comment) | `shell` | `tools`, `gate` (`real`, `secret`) | `gate` |
| `src/taint.rs` | Cross-cutting handling of untrusted external content (web, MCP, subagents, history, untrusted project files) and of what tainted sessions stored (D-053): marker defusing and one `<untrusted>` envelope, session provenance (`sessions.risky_at` plus `risky` tape block; held effects set `sessions.tainted_at` and a `taint` block; an error when it can't be recorded, and the content is withheld, `withheld`), `hand_over_rating` (the one read path for stored content: wrapped and raising the reader by its numeric rating, level 5 denies it), `deny`, `trust`, `limited` (the one place the rules for a tainted session are decided: `Limit::Steer`, `Vouch`, `StartJob`, `Shell`), `shell_sandbox`, and centralized tainted-state lookups (a database error counts as tainted) | `untrusted`, `taint`, `withheld`, `hand_over_rating`, `deny`, `trust`, `stopped`, `Limit`, `limited`, `shell_sandbox`, `tainted` (risky provenance), `prefix_tainted`, `escalate` | `tape`, `tools` (`Sandbox`) | `web`, `mcp`, `search`, `wiki`, `memory`, `delegate`, `agent`, `workshop`, `dispatch`, `jobs` |
| `src/mcp.rs` | MCP client, written against the spec (2025-06-18; no `rmcp`): servers from `~/.zenbot/mcp.json` (`ZEN_MCP_CONFIG`; `mcpServers`, `command`/`args`/`env` for stdio or `url`/`headers` for streamable HTTP, `${VAR}` from the kernel's environment, a secret-named one (`secrets::is_secret_var`) only when the server lists it in `allow_env`, else left empty and reported; per server `enabled`, `timeout_s`, `include`/`exclude`, `allow_env`, `untrusted`, default true for every server, D-052). Config re-read when its mtime changes; servers connected lazily (all at once), reconnected after a transport error, and retried at most once a minute while down; a bare tool name must be unique. Calls run concurrently: a server's lock is held only to connect, its tools come from the last listing (again on `tools/list_changed`), a stdio server's requests are written under a stdin lock and a reader task hands answers out by id (a timeout restarts it only when no other call is running); a stdio line or HTTP body over 8 MB fails the call; stdio servers run in their own process group, killed whole (`tools::GroupKill`). The model's tool list stays fixed: `find_tools` (word ranking, names as `<server>_<tool>`), `load_tool` (schema as a tool result), `call_tool` (required arguments checked, output masked, over 50 KB cut with the full text in `~/.zenbot/outputs/mcp-*.txt`; untrusted servers' output wrapped and the session tainted). Untrusted descriptions and schemas are wrapped and taint the session; no lock is held during server calls. The catalog also lists the agent's own tools as server `made` (`workshop::made_entries`); catalog/load/validation retain the exact manifest's maximum numeric origin and writer edges, withholding contamination; calling one runs `workshop::run_made` | `config_path`, `expand`, `parse_config`, `sse_messages`, `rank`, `missing_args`, `result_text`, `find_spec`, `load_spec`, `call_spec`, `run_tool`, `status` | `taint`, `secrets`, `tape`, `workshop` | `agent`, `api` (`/api/mcp`), `secrets` (`config_path`) |
| `src/tools.rs` | The file and shell tools and their execution: `bash` on `run_shell` (own process group killed on timeout or abort; the kernel's secret variables removed from the shell's environment (`secrets::is_secret_var`); `Sandbox`: read-only `bwrap` for a verifier (its own pid namespace and session, no network, an empty home with only the work bound back, the secret files (`secrets::secret_files`) hidden), offline `bwrap` for a tainted session (the same, but the home read-only instead of empty, `/run` emptied, the workspace bound back read-only, the zen home read-only again inside it; `OFFLINE_NOTE` ends the output; shell sandbox modes also apply a seccomp connection/datagram/io_uring filter to close host Unix-socket escapes), or none; every mode masks `fsview` masks, and a host shell with masks runs in its own pid namespace with no Unix sockets (`unix_filter`); also used by `verify`'s criteria checks (in the session's sandbox) and, unsandboxed, the `engines` job), `read` (16 MiB file cap; a verifier's confined to its workspace, secret files refused, `confined`), `write`, `edit` (whitespace/line-ending normalized matching without adding a trailing newline; mixed line endings kept); `write` and `edit` replace files atomically (temp file and rename, symlinks followed, permissions kept). Per-file locks. Output over 50 KB cut to head and tail (off the runtime threads), full text saved under `~/.zenbot/outputs/`. Results are masked at the dispatcher | `specs`, `execute`, `Sandbox`, `OFFLINE_NOTE`, `run_shell`, `Shell`, `GroupKill`, `resolve`, `save_full_output`, `ToolOutput` | `secrets` | `dispatch`, `agent`, `jobs`, `context`, `web`/`mcp` (`ToolOutput`) |
| `src/fsview.rs` | What a shell may see (D-064): `prepare` (before the gate) finds masks, the canonical paths whose current bytes match by digest (wherever registered) a contaminated or level-5 rating, paths rated `*` (a product it couldn't read), other hard links in the workspace (refuses when some are elsewhere), and the rated workspace files a shell can show (digests and lines cached by inode and ctime); `View::exposure` rates a bash result by the files whose lines it shows, `rate_all` rates verify's whole view; `finish` registers what a host shell wrote (ctime after start; build/cache folders skipped) at the session's risk and withholds the result when the scan is incomplete, a name isn't UTF-8 or the session was stopped; `mask` (a directory gets an empty tmpfs) | `prepare`, `View`, `record`, `finish`, `mask`, `digest` | `sources`, `taint` | `dispatch`, `agent`, `tools` |
| `src/compile.rs` | What a turn sends, in cache order: envelope (system prompt + tools, stored once in `envelopes`) → summary → history → prompt with turn context. D-062 exact-byte snapshots (`admitted`) and writer dependencies admit only origin 0–1 prompt/project text, withholding contamination or metadata failures. The system prompt: SOUL.md, IDENTITY.md, AGENTS.md (placeholders filled), USER.md (each capped, cut in the middle), memory and the sleep note, the skills index, project instruction files | `system_prompt`, `file_cap`, `cut_middle`, `PARTS`, `compose` (only the parts a scheduled job picks), `base_prompt`, `envelope`, `turn_context`, `history`, `Envelope`, `SummaryRef` | `context`, `memory`, `skills`, `tape` | `turns`, `measure`, `jobs`, `agent` (`file_cap`) |
| `src/context.rs` | Instruction files (AGENTS.md, else CLAUDE.md, capped at 32 KB): always one per directory from `/` to the workspace (`~/.zenbot/AGENTS.md` is a prompt file, compile.rs); on demand, a project's file the first time a tool touches a path in it (excluding paths under the zen home) (tape kind `context`): as instructions only at exact-byte origin 0–1 inside a trusted repository (`ZEN_REPO`, `ZEN_TRUSTED_REPOS`) or a git worktree of one (linked both ways, D-056), else as untrusted content | `always`, `governing`, `paths_in_call`, `trusted_roots`, `trusted`, `attachment`, `untrusted_attachment`, `capped`, `rate`, `admit`, `attach_rating` | `tools::resolve`, `taint::untrusted` | `compile`, `dispatch` |
| `src/compact.rs` | Summaries of older turns (the summarizer reads the owner's words whole, the rest cut to fit 400 KB): linear-time plan, prepared in the background past the soft limit, applied after a pause or at once past the hard limit (`compaction` block). Also the `history` tool, which reads old blocks back by number or search, in this session or another (`session`: a full id or a unique prefix, e.g. the 8 characters `search` shows; a prefix is looked up as the primary-key range it covers, `id_prefix_range`) | `Settings`, `summary_model`, `plan`, `prepare`, `pending`, `apply`, `tool_spec`, `history_tool`, `id_prefix_range` | `tape`, `workers::complete` | `turns`, `dispatch`, `tools` |
| `src/measure.rs` | Per-turn record of what was sent and why the prompt cache could or couldn't be reused (`cache_break`: `first`, `instructions`, `summary`, `model`, `engine_session`, `expired`; unexpected: `history`, `miss`) | `previous`, `record`, `break_at_start`, `break_at_end`, `cache_ttl_secs` (`ZEN_CACHE_TTL` through `config`) | `compile` types, `config` | `turns` |
| `src/tape.rs` | D-062 ratings/provenance appended under the chain lock, session max updated, owner provenance only through `append_owner`; tool metadata requires matching kernel result digest, tool name and error status and retains the exact tool record. The tape: append (advisory lock per session, `seq` + parent + hash computed in SQL by `zen_block_hash`), load, repair (`zen_rechain`) | `Block`, `append`, `load`, `load_all`, `repair` | DB functions from migration 0007 | `compile`, `compact`, `agent`, `dispatch`, `turns`, `jobs`, `taint`, `assist`, `main.rs` |
| `src/workers.rs` | Worker configs from `ZEN_WORKERS` (empty means `engine`; a name without `ZEN_WORKER_<NAME>_CMD` is ignored with a warning and reported by `/health`), supervision with backoff (ends orphaned turns on a crash), model routing and metadata rebuilt on refresh (a worker whose `models.list` fails keeps its previous models, `kept_models`), all worker models visible by default (`ZEN_MODELS` may curate), effort checks, `complete` for summaries | `Worker`, `worker_configs`, `forward_worker`, `supervise`, `complete`, `json_object` (the JSON object in a completion's text, for `compact` and `assist`), `visible_models`, `collect_models`, `worker_for`, `model_info`, `check_effort` | `mind`, `score` | `main.rs`, `api`, `turns`, `agent`, `compact`, `assist`, `delegate` |
| `src/mind.rs` | JSON-RPC client for one worker process, sending `zen_proto::worker::Request`s (`bash -c <cmd>`, without any secret-named variable of the kernel's (`secrets::is_secret_var`) but the engines' sign-ins, `scrubbed`); 30 s default request timeout (`call`), `call_within` for longer or shorter; a request's entry is removed however it ends; lines are read as bytes (invalid UTF-8 can't stop the reader) | `Mind`, `Incoming`, `spawn`, `call`, `call_within`, `respond` | `secrets` | `workers`, `main.rs` |
| `src/score.rs` | Direct OpenRouter System One call and live scoring: a classifier answers fixed, versioned questions (`QUESTIONS_VERSION = "v1"`) about a session after a decision or after it goes idle; stored in `session_scores`. Off unless `ZEN_S1_MODEL` is set. Also `decide` (any typed question to System One), `judge` (`Relevance`: reranking and `focus`, logged under their own `point`) and `private_ok` (`ZEN_S1_PRIVATE`) | `scorer`, `openrouter_key`, `decide`, `judge`, `private_ok`, `questions`, `state`, `score_session`, `idle_loop` | OpenRouter `/api/v1/systemone` | `api`, `agent`, `memory`, `web`, `search`, `wiki`, `workshop`, `delegate`, `jobs`, `workers`, `main.rs` |
| `src/secrets.rs` | Masks secrets in tool output: values of the kernel's own secret-looking env vars, `~/.zenbot/token`, `~/.zenbot/db-password`, `~/.codex/auth.json` and `~/.claude/.credentials.json` values (reloaded when those files change), and well-known token prefixes / private key blocks | `mask`, `mask_off_thread` (large text on a blocking thread), `is_secret_var` (named like a token, key, secret or password, or `DATABASE_URL`: masked and kept out of the agent's shells), `secret_files` (`token`, `db-password`, `env`, `env.prev`, `mcp.json`, `matrix.env`, `matrix/` under the zen home and the engines' sign-ins: hidden from a verifier's shell and refused to its `read`) | `mcp` (`config_path`) | `tools`, `mind` (`is_secret_var`), `agent` (check output, diff), `memory` (memories), `web` (pages), `mcp` (tool output) |
| `src/test_util.rs` | Test-only owner-private scratch directory removed on drop; `bash` large-output tests save there instead of the live zen home | `TestDir` | — | kernel unit tests |
| `src/update.rs` | Self-update: compares `~/.zenbot/version` with `origin/main` (hourly by default), asks `scripts/fetch-release.sh --check` whether binaries exist (30-second timeout), starts `scripts/self-update.sh` on request, keeping the last 64 KB of its output | `Updater` (`running`, `info`, `check`, `check_periodically`, `start`, `status`) | `git`, scripts | `api`, `main.rs` |
| `src/git.rs` | The one place the kernel runs git: async, with a 60 s limit and an output cap (git is stopped once the cap is reached); `run` for an effect, `commit_all` commits a folder as `zenbot` (making it a repository first) for the wiki, skills and made tools | `output`, `git`, `run`, `commit_all` | — | `update`, `agent` (the verifier's diff), `wiki`, `workshop` |
| `src/config.rs` | The kernel's tunable settings, read once at start into a typed `Config` with every default in one place (CONTEXT.md principle 5); a value that doesn't parse runs on its default (`unparsed`, logged by `main`), a flag is off with `0`/`off`/`false`/`no`; `cache_ttl_secs` per engine (`ZEN_CACHE_TTL`). Boot settings, paths, workers' and keys are still read where used (ROADMAP.md, debt) | `get`, `Config`, `Config::read`, `cache_ttl_secs` | — | most modules |
| `steps/verify.md` | The verifier's whole system prompt (`include_str!` in `agent.rs`) | — | — | `agent::system_for` |
| `src/layout.rs` | Where files live under the zen home, by scope (D-040): system-wide prompt files at the top, `agents/<name>/` (the agent's `SOUL.md` and `IDENTITY.md`), `global/` (`MEMORY.md`, wiki, skills, tools). `migrate` moves an old flat layout once at startup, before the defaults, never overwriting, and leaves a relative symlink at each old path for rollbacks | `SOUL`, `IDENTITY`, `global_dir`, `migrate`, `MOVES` | — | `main.rs`, `compile`, `defaults`, `memory`, `wiki`, `skills`, `workshop` |
| `defaults/` | Default `SOUL.md`, `IDENTITY.md`, `AGENTS.default.md` (installed as `AGENTS.md`; named so sessions in the repo don't load it as the project's instructions; with `{{workspace}}`, `{{home}}`, `{{zen_home}}`, `{{repo}}`), `USER.md`, and the skills `work/brief` (with `references/template.md`), `work/verify` and `work/close` (keep what a job taught: memory, wiki, skills, tools) (`include_str!` in `defaults.rs`) | — | — | `defaults::install` |
| `migrations/*.sql` | Schema (see Database) | — | — | `sqlx::migrate!` in `main.rs` |

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
  markers defused) and taints the session, so what that session saves to memory counts as inference;
  what a tainted session stores is marked and reaches any session only through `taint::hand_over_rating`.
- `mcp.rs`: MCP tools never join the model's tool list (that would break the prompt cache); they
  are reached through `find_tools` / `load_tool` / `call_tool`, and every call goes through the
  kernel like any other tool call.
- `secrets.rs`: masking applies to what tools return; the command text the model typed is not masked.

---

## Kernel routes

From `main.rs` (router) and `api.rs`. Everything under `/api` needs the token in `Authorization: Bearer` (never in the URL).

| Method | Path | Handler | Does |
|---|---|---|---|
| GET | `/health` | `health` | No auth. `{ok, db, mind, workers:{name:bool}, ignored_workers, busy, version, commit}`; pings every worker; `ignored_workers` (names in `ZEN_WORKERS` that aren't known workers) don't affect `ok`. `busy` = running turns + kernel work outside them. Used by `wait_healthy` and `apply-upgrade.sh` (`"busy":0`) |
| GET | `/api/models` | `list_models` | Asks every worker for `models.list`, refreshes routes, returns all worker models by default (`ZEN_MODELS` optionally curates, plus any `faux/*`), `authenticated`, `default`, `scorer` |
| GET | `/api/sessions?archived=` | `list_sessions` | The owner's sessions and scheduled jobs' sessions (`kind IS NULL OR kind = 'job'`; subagents, verifiers and kernel sessions hidden), with cost |
| GET | `/api/board?archived=` | `board` | The latest 200 sessions (archived ones only when asked; otherwise counted) and the latest 20 children of each plus running ones, newest activity first, with `kind`, `parent`, `busy`, `waiting` (unanswered `ask` questions), `children` (a count) and, for child sessions, `task` (first message); no cost. zen's sessions board polls it |
| POST | `/api/sessions` | `create_session` | `{title?, model?, effort?}` |
| GET | `/api/sessions/{id}?last=&after=` | `get_session` | Session, its `message` blocks (with `seq`; all, the `last` N, or those after `after`), `message_count`, `tokens` and `busy` |
| PATCH | `/api/sessions/{id}` | `update_session` | Title, model, effort (`"default"` clears it), archived |
| GET | `/api/sessions/{id}/ws` | `session_ws` | WebSocket; its session's event hub is created on connect and freed when the last client leaves. Client sends `{type:"prompt",text}` or `{type:"abort"}`. A `prompt` may carry `suggestion: {id, taken}` (what the owner did with the suggested prompt). Server events: `message`, `delta`, `thinking`, `tool_start`, `tool_end`, `busy`, `end`, `status`, `questions`, `taint`, `child_end`, `title`, `suggestion`, `error`, `resync` |
| GET | `/api/sessions/{id}/taint` | `session_taint` | What marked the session since the last trust (`taint::report`): `tainted_at`, `taints` (its `taint` blocks), `sources` (untrusted envelopes in its messages by source and about: first block, count), `checked` (the tape's last block), `busy` |
| POST | `/api/sessions/{id}/trust` | `trust_session` | `{seen}`: the owner lifts the taint (`taint::trust`, D-054): a `trust` block, `taint` event; 409 while a turn runs or when it read untrusted content after `seen`; `{trusted: false}` when it's clean |
| POST | `/api/sessions/{id}/deny` | `deny_session` | Owner-only stop: session and causal descendants/products receive reversible contamination marks (D-062); no model tool exposes deny/trust |
| POST | `/api/sessions/{id}/decision` | `decide` | `{decision: accept\|more\|reshape\|drop, note?}` → `session_decisions`; triggers scoring; `accept` activates draft skills the session loaded (`workshop::on_accept`) |
| GET | `/api/memory?tier=` | `list_memory` | Memories of a tier (`short` by default, `archived`, `all`; `long` and `proposed` only hold rows from before D-045), the last sleep and the size |
| POST | `/api/memory/sleep?trigger=` | `run_sleep` | Tidy short-term memory now (`trigger=nightly` is recorded as the nightly sleep, else `owner`; the scheduled `sleep` job calls `memory::sleep` directly) |
| GET | `/api/suggestions` | `assist::stats` | Suggested next prompts: outcomes (`accepted`, `edited`, `declined`, `unseen`, open) by prompt version and model, latency and cost; the latest 20 |
| GET/POST | `/api/jobs` | `list_jobs` / `create_job` | Every job (`jobs`, `scheduler` on/off); POST `{name, prompt, schedule, tz?, context?, skills?, model?, workspace?, report_file?, post_to?}` creates an agent job as the owner |
| PATCH/DELETE | `/api/jobs/{name}` | `update_job` / `delete_job` | `{enabled}` pauses or resumes (resume approves an agent's paused job); other fields change an agent job (`report_file: ""` and `post_to: ""` clear them); DELETE removes an agent job (runs kept) |
| POST | `/api/jobs/{name}/run` | `run_job` | Run a job now in the background (`{run}`; 400 when it's already running) |
| GET | `/api/jobs/runs?job=&limit=` | `list_job_runs` | Recent runs, newest first: status, trigger, session, report or error |
| GET | `/api/mcp` | `mcp_status` | `{config, servers:{name:tool count}, problems}` from `~/.zenbot/mcp.json` (the agent's own tools count as server `made`); connects servers not yet connected |
| GET | `/api/policy` | `get_policy` | The routing policy in force (`version`, `policy`), the evidence (`stats`) and what it suggests |
| POST | `/api/policy` | `put_policy` | `{policy: {routes, explore?}, reason?}`: a new version by the owner |
| POST | `/api/policy/undo` | `undo_policy` | A new version with the data of the one before the latest (409 when there is none) |
| GET | `/api/skills` | `list_skills` | Skills (active and drafts) with loads, last load, accepted and judged sessions that loaded them, and each draft's `sha` (its fingerprint); the agent's tools, whether each is approved, and their `sha` |
| POST | `/api/skills/review` | `review_skill` | `{name: "domain/name", decision: accept\|reject, sha}`: activate a draft skill or move it to `_archived`; `sha` is the fingerprint the owner reviewed (8+ characters), 400 when the draft changed since |
| POST | `/api/tools/{name}/review` | `review_tool` | `{decision: accept\|reject, sha}`: `accept` approves a made tool (network, no sandbox), `reject` rejects it; `sha` as for skills, 400 when the tool changed since |
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
| `bash`, `read`, `write`, `edit` | `tools.rs` (bash in a read-only bwrap sandbox for a verifier, its `read` inside its work; `write`/`edit` on protected files refused by `agent::protected_refusal`) | every session (a verifier: `bash`, `read`) |
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

### `zen-engine` (`crates/zen-engine`)

| File | Role | Depends on | Used by |
|---|---|---|---|
| `src/main.rs` | Entry: `zen-engine` serves the protocol on stdio (requests read as `zen_proto::worker::Request`); `zen-engine mcp-bridge <socket>` runs the bridge; `zen-engine codex-features` checks Codex's default features (exit 1 on one zenbot doesn't know). Routes `turn.start`/`complete` to an engine by model prefix (`engine::for_model`), lists every engine's models, makes one safe cross-provider quota continuation when eligible, settles the turn's tool calls before `turn.end`; running turns keyed by `turn_id` (`Turns`); `turn.start` and `turn.abort` handled in arrival order, turn ids aborted before their start remembered (256) | all modules | `zend` (`ZEN_ENGINE_CMD`), Claude CLI (bridge) |
| `src/engine.rs` | The `Engine` trait each engine implements (`name`, `version`, `models`, `run_turn`, `complete`, `prepare` at start-up, `subscription`, `failover_target`, `subscription_login`) and the registry (`all`: claude, codex, faux when `ZEN_FAUX=1`; `find`, `for_model`). Adding an engine is a module implementing it and its line in `all` | `claude`, `codex`, `faux` | `main.rs`, `failover` |
| `src/rpc.rs` | JSON-RPC over stdio with the kernel | — | everything |
| `src/turn.rs` | Shared per-turn pieces: `TurnInput` (from `turn.start`), `TurnCtx` (sends the turn's `TurnEvent`s with its session and turn ids, transcript and usage capture, in-flight tool safety tracking; a Unix socket per turn forwards tools to the kernel as `tool.call`; `settle` records tool calls left without a result as interrupted; `engine_result` records a result the engine gave without the kernel; `claim` waits 2 s for a call's announcement, else counts it unmatched, which fails the turn), `COMPLETE_LIMIT` (540 s), `StderrTail` (last few KB of a CLI's stderr, quoted in errors), history transcript, seed blocks, `engine_dir` (private folders under `<zen home>/engine/`: `claude`, `codex`, `complete`, `sockets`), `enabled(var)` (on unless `"0"`) | `rpc`, `zen-proto` | `claude`, `codex`, `faux` |
| `src/bridge.rs` | The stdio MCP server Claude Code launches; answers `tools/list` and `tools/call` by asking the engine over the socket | `turn` socket | Claude CLI |
| `src/claude.rs` | `Claude` (the `claude` engine): runs a turn with `claude -p` (stream-json), `--tools ""`, `--strict-mcp-config`, `--setting-sources ""`, `--system-prompt-file` (zenbot's prompt in a private per-turn file, not on argv), allowed tools `mcp__zen__*`; a run (turn or `complete`, both stream-json) is refused unless `system/init` comes first with a tool list holding only `mcp__zen__*` (`check_init`). Tool results the CLI gave itself (in `user` events) go on the tape; the tool socket server stops and `turn.usage` is sent however the turn ends; `complete` is killed after `COMPLETE_LIMIT`. The tool socket lives in a private per-turn folder under the engine home. Engine sessions run in the fixed directory `<zen home>/engine/claude` (Claude Code files them under `$CLAUDE_CONFIG_DIR/projects/`; resume / seed / transcript); unused ones deleted after 30 days. `complete` runs in `<zen home>/engine/complete`. Fails the turn on stream lines it doesn't understand, and reports `fallback_reason` | `turn`, `bridge` | `engine` |
| `src/codex.rs` | `Codex` (the `codex` engine): runs a turn through `codex app-server` (cwd: an empty private dir, `<zen home>/engine/codex`, also for model listing and `complete`): features in `DISABLED` off; a turn or completion in which Codex starts an item not in `ITEMS` (message, reasoning, plan, compaction, dynamic tool call) is stopped (`foreign_item`); `check_features` (`zen-engine codex-features`) fails on a default-on feature in neither `DISABLED` nor `REVIEWED`; threads kept unless `ZEN_CODEX_RESUME=0`, history injected as native items (`thread/inject_items`, fallback transcript), zenbot tools as dynamic tools; a private `CODEX_HOME` links only the owner's sign-in (from `$CODEX_HOME`, else `~/.codex`). Fails loudly: refuses app-server requests it doesn't handle (ending the turn), errors quote stderr, a turn with no messages fails, `fallback_reason` reported; model listing within 15 s, kept for an hour or until the `codex` binary changes (a failure returns the previous list; no lock held while asking); `initialize` within 15 s; `complete` killed after `COMPLETE_LIMIT`; an abort also stops a hung app-server start or thread setup, and an aborted turn still reports its usage; `codex --version` asked again when the binary changes | `turn` | `engine`, `main.rs` (`codex-features`) |
| `src/failover.rs` | One safe cross-provider continuation on a hard subscription quota error (D-044; off with `ZEN_QUOTA_FAILOVER=0`); only on a subscription sign-in (each engine's `subscription_login`, within 5 s; refused when `ANTHROPIC_API_KEY`/`CLAUDE_CODE_OAUTH_TOKEN` or `OPENAI_API_KEY`/`CODEX_ACCESS_TOKEN` is set, so it never bills an API key); the target is the engine's `failover_target` (on another subscription engine that lists it); carries current-turn history without repeating uncertain tool calls, and sends `turn.fallback` | `turn`, `engine` | `main.rs` |
| `src/faux.rs` | `Faux` (the `faux` engine): scripted models `faux/smoke` and `faux/backup` (when `ZEN_FAUX=1`), steps from `ZEN_FAUX_SCRIPT` (list, or object keyed by kind of session: `verify` for a verifier, `default`): `tool`, `text`, `sleep`, `exit`, `fallback` (sends `turn.fallback` to a model); modifiers `when` (only if the prompt contains the text) and `ignore_abort` | `turn` | `engine`; e2e, upgrade smoke test, evals |

Code that depends on a CLI's flags or output should fail loudly, so the daily engine update check
(`scripts/update-engines.sh`) catches a breaking CLI release.

---

## Layer 3 — CLI `zen` (`crates/zen`)

| File | Role | Depends on | Used by |
|---|---|---|---|
| `src/main.rs` | Entry and `run`: dispatches each command (plain `zen` opens the sessions board; piped input without a command is `zen ask`) | `cli`, `client`, `turn`, `print`, `setup`, `tui` | owner, scripts (`zen ask --json`), e2e, evals |
| `src/cli.rs` | clap arguments and subcommands: `ask`, `chat`, `sessions {ls,new,show,archive,restore,rename,decide}`, `memory [--tier short\|archived\|all] [sleep]`, `jobs [add\|set\|pause\|resume\|rm\|run\|runs]`, `skills [accept\|reject <domain/name> <sha>]` (use per skill and the made tools, each draft's and tool's fingerprint; accepting or rejecting names the fingerprint reviewed), `tools accept\|reject <name> <sha>` (a `made_` prefix is dropped), `policy [set <kind> <model> [--candidates a,b] [--explore x] \| undo]` (the routing policy and its evidence), `suggestions` (what became of suggested prompts), `models`, `login [claude\|codex]`, `status` (with memory, jobs and engines lines), `upgrade [--check]`; flags `--url` (`ZEN_URL`), `--token` (`ZEN_TOKEN`), `--json`, `-c`, `-r`, `-m`, `-e`, `--inline` (`ZEN_INLINE`) | clap | `main.rs` |
| `src/turn.rs` | `zen ask` and line-by-line `zen chat`: stream one turn over the session WebSocket as `zen_proto::client::Event`s (Ctrl-C stops it, a second one leaves) and report it (`--json`: text, tools, the kernel's turn records) | `client`, `print`, `zen-proto` | `main.rs` |
| `src/setup.rs` | Reads `<zen home>/env` (for `ZEN_REPO`, `PATH`; `zen_client::parse_env`) and `<zen home>/engines.json`; engine sign-in (`zen login`), git identity and the root filesystem's use for `zen status` (`root_disk`: `df -Pk /`; `disk_line` warns over 80%) | `client` | `main.rs`, `tui/` |
| `src/print.rs` | How script commands print: `emit` (JSON or text), sanitized stdout/stderr, sessions, messages, memory, jobs and suggestions lines | `client`, `md` | `main.rs`, `turn.rs` |
| `src/client.rs` | The shared client (`zen-client`, re-exported) plus what only zen needs: `NewSession`, `upgrade` (wait/poll messages), `describe_update`, display helpers (`short`, `dim`, token totals, `tool_summary`) | `zen-client` | `main.rs`, `turn.rs`, `tui/` |
| `src/tui/` | Interactive app split into `mod` (lifecycle), `state` (sessions/reconnect), `events`, `input`, `render`, `transcript` (incremental streaming and view), `board` (the sessions board, zen's home screen, fetched in a task of its own), `panel`, `pickers`, `inline`, `files` and `test_util`. Full screen has a diffed, scrollable view and side panel; inline keeps scrollback. Terminal text is sanitized, requests fail into notices rather than exiting, and reconnect rebuilds from the server. Prompt history is private JSON lines in `<zen home>/history`. Render/key tests live beside the modules | `client`, `editor`, `md`, `setup` | `main.rs` |
| `src/screen.rs` | Full-screen frames: writes only changed rows in place; `fit` cuts/pads styled lines; `row` joins chat and panel | `md` | `tui/render.rs` |
| `src/editor.rs` | Multi-line input editor with prompt history | — | `tui/` |
| `src/md.rs` | Styled lines, word wrap, line-oriented markdown renderer, terminal text sanitizer | — | `tui/`, `print.rs` |

UI changes to `tui/` / `editor.rs` come with render or key tests (AGENTS.md).

## Shared: `zen-proto` (`crates/zen-proto`)

`lib.rs`: `text_of` (message content as text, in the message format the tape stores), `head`,
`tail`. Used by `zend` (`api`, `agent`, `assist`, `compact`, `delegate`, `jobs`, `mcp`, `memory`,
`score`, `search`, `web`, `wiki`, `workshop`), `zen-engine` (`turn`), `zen` (`turn`, `print`,
`tui`) and `zen-matrix` (`relay`). Changing how content is read changes it in all four.

`client.rs`: the client protocol's session events, typed (`Event`, one variant per event in
docs/client-protocol.md; an unknown or misshapen one is `Other` with its JSON), `Event::ends_turn`
(the one end-of-turn rule: `end` without `next`, `idle`, `resync` with `busy: false`), and echo
matching for a sent prompt (`echoes`, `Sent`). Used by `zen` (`turn`, `tui/events`) and
`zen-matrix` (`relay`, `main`). The kernel still builds its events with `json!`; a change to an
event changes docs/client-protocol.md, this enum and its tests together.

`worker.rs`: the worker protocol's messages, typed (docs/worker-protocol.md): `Request` (the
kernel's `ping`, `models.list`, `turn.start` (`TurnStart`, `Resume`), `turn.abort`, `complete`),
the answers (`Pong`, `Ack`, `ModelsList` with `Model`, `Completion`, `CompleteError`), `ToolCall`
and `ToolResult`, and the turn notifications (`TurnEvent`; `TurnRef` holds the ids each carries).
Reading is lenient (unknown fields ignored, missing or `null` ones defaulted); messages and usage
reports stay JSON. Used by `zend` (`mind`, `dispatch`, `turns`, `workers`, `api`) and `zen-engine`
(`main`, `engine`, `turn`, `failover`, the engines). A protocol change changes the doc, these types
and their round-trip tests together.

## Shared: `zen-client` (`crates/zen-client`)

The kernel client both clients use: `zen_home` (`ZEN_HOME`, else `~/.zenbot`), `parse_env` (the
`env` and `matrix.env` files), `Client` (the token from the caller or `<zen home>/token`, in the
`Authorization` header; `call`/`get`/`post`/`patch`/`delete`, `health`, `new_session`, `resolve`,
`connect` to a session's WebSocket; 5 s connect and 120 s request timeouts; a warning when the
token would go over plain http to another machine), `enc` (a path segment, percent-encoded), `Ws`.
`owner.rs`: the owner's chat commands (`jobs`, `skills`, `tools`, `trust`: parse, call `/api/jobs…`,
`/api/skills…`, `/api/tools/{name}/review`, `/api/sessions/{id}/taint` and `…/trust`, answer as
Markdown; `Chat` keeps the prefix and `trust`'s last look per session), used by zen's `/jobs`,
`/skills`, `/tools`, `/trust` (`tui/input.rs`) and zen-matrix's `!jobs`, `!skills`, `!tools`,
`!trust` (`kernel.rs`).
Used by `zen` (`client`, `setup`) and `zen-matrix` (`kernel`, `state`, through a path dependency).

---

## Channel: `zen-matrix` (`crates/zen-matrix`, its own Cargo workspace)

A separate process, a client of the client protocol (D-049; docs/matrix.md has the file table).
Not in the root workspace or CI's release tarball: `scripts/matrix.sh` builds and installs it, and
`apply-upgrade.sh` doesn't upgrade it. Settings come from `~/.zenbot/matrix.env` (the process
environment wins; see Environment variables).
`main.rs` (commands `login`/`run`/`status`/`repin-owner`, the Matrix client, rooms, invites,
membership watch, posting, per-room link tasks with catch-up, job reporter), `trust.rs` (pure: who
and where it answers: sender, device trust, room members and encryption, invites, the pinned owner
identity), `relay.rs` (pure: commands, events → posts, numbered answers, job reports, splitting,
Markdown → HTML without raw HTML; session events read as `zen_proto::client::Event`),
`kernel.rs` (the API calls it uses, over `zen-client`: `POST /api/sessions`,
`GET /api/sessions/{id}?after=` and `?last=1`, the session WebSocket, `GET /api/jobs/runs`, and the
owner commands through `zen_client::owner`),
`state.rs` (`matrix.env` read with `zen_client::parse_env`, `state.json`, private files). `examples/owner.rs`: the scripted owner for
`scripts/matrix-e2e.sh`. The kernel knows nothing of it.

---

## Database

Postgres 16 with pgvector (`pgvector/pgvector:pg16`), user and database `zen` on `127.0.0.1:5432` (password: `~/.zenbot/db-password` once `db_secure` ran; `zen` before)
(`deploy/compose.yaml`). Migrations in `crates/zend/migrations/`, applied by `zend` at start.

**Migrations are expand-only** (AGENTS.md): new file per change, never edit an applied one; add
tables, columns, indexes; don't drop, rename or retype anything in the release that stops using it.
A rollback swaps binaries, not the schema, and `zend` runs with `set_ignore_missing(true)` so an
older build starts on a newer schema.

| Table | Created / altered | Purpose | Written by | Read by |
|---|---|---|---|---|
| `sessions` | 0027 risk/max and trust watermark; 0001; `effort` 0003; `state`, `parent`, `kind` 0011; `workspace` 0013; `tainted_at` 0016; `title_source` 0020 (`auto`, `model`, `owner`: the assist model never renames an `owner` title); job sessions from before D-057 archived 0025; model ids rewritten 0002 | One per session (one job). `kind` is NULL for the owner's sessions; `'verifier'` or `'subagent'` for child sessions (`parent` set; also `'subagent'` with no parent for a kernel session, e.g. the sleep compacting a prompt file), `'job'` for a scheduled job's run (no parent; listed with the owner's sessions). A child inherits the parent's `tainted_at`, and a subagent may get its own `model`; `workspace` overrides the kernel's; `tainted_at` = when it first read untrusted content (web, untrusted MCP servers); `state` (legacy, no longer written) | `api` (create/update), `turns` (child and kernel sessions), `jobs` (job sessions), `assist` (title, `title_source = 'model'`), `tape::append` (`updated_at`), `taint` (`tainted_at`) | `api`, `turns`, `assist`, `taint::tainted` (many modules), e2e |
| `tape_events` | 0027 risk/origin and exact tool record reference; 0001; `seq`, `parent`, `hash` + functions `zen_block_hash`, `zen_rechain` 0007; index `(session_id, kind, seq)` 0014 | The tape. Kinds: `message`, `base`, `envelope`, `context`, `compaction`, `engine_session`, `questions`, `verdict`, `verification`, `taint`, `failover`; from the old workflow, no longer written: `state`, `brief`, `approval`, `ruling`, `submission`, `report` | `tape::append` only (callers: `dispatch`, `turns`, `jobs`, `compile`, `compact`, `agent`, `taint`) | `tape::load*`, `compact`, e2e (`tape_is_sound`) |
| `model_calls` | 0001; `turn_id`, `duration_ms` 0004; index 0012; partial index on untraced calls (`turn_id IS NULL`) 0014 | One row per assistant message: tokens, cache, cost | `dispatch` (`turn.message`) | `turns`, `measure`, `session_cost` |
| `item_ratings`, `contamination` | 0028 | Versioned exact-byte origin ratings and reversible per-cause session contamination; stored memory/report triggers retain numeric origin | `sources`, `taint`, DB triggers | stored reads, prompt admission, delivery |
| `session_inputs` | 0029 | Reader/writer edges; recursive contamination follows parents and recorded inputs, including late edges | `sources`, `jobs`, history | `zen_session_contaminated`, `taint` |
| `sources`, `source_audit`, `source_vouches` | 0027 | Owner allowlist and immutable change audit; exact owner-page rating adjustments cached by message sequence and base | `sources` (owner API changes only) | `sources`, `gate`, `web` |
| `tool_calls` | 0027 risk/origin/result digest and full masked source arguments; 0001; `turn_id` 0004; index 0012; index on `name` 0021 | One row per tool call; `args` has secrets masked and each string cut at 4 KB, and a cut call keeps the sha256 and size of its whole (masked) arguments under `_full` | `dispatch` (`tool.call`) | `turns`, `workshop` (`load_skill` calls: skill use, `on_accept`) |
| `turns` | 0004; `envelope`, `context`, `context_tokens`, `cache_break` 0009 | One per turn: harness, worker, engine, model, effort, outcome, totals, what was sent | `turns` (`begin_turn` insert, `record_turn` update) | `api`, `measure`, `score`, `turns`, e2e, evals |
| `session_decisions` | 0005; `source` 0011 | The owner's verdict (`accept/more/reshape/drop`; ground truth); `source` was also `model` under the old workflow's auto-close | `api::decide` | `workshop::stats` (accepted sessions per skill), `delegate::stats` (the parent's verdict judges its subagents), e2e (scoring compares against it later) |
| `session_scores` | 0006; index `(turn_id, trigger)` 0014 | System One answers about a session | `score` | `score` |
| `envelopes` | 0008 | System prompt + tools, stored once per distinct pair, keyed by hash | `compile::envelope` | `compile` |
| `compactions` | 0010 | How each summary was made; applied ones have `applied_seq` | `compact` | `compact`, e2e |
| `decisions` | 0011 | System One decisions and what was done, by `point`: `tool` (the `decide` tool), `sleep` (the sleep's fate for each memory), `web_rerank` (web search results ordered), `web_focus` (parts of a page kept), `search_rerank` (`search` results ordered), `capture` (the wiki page a note goes to, whether it's known or sensitive), `skill_duplicate` (whether a new skill does an existing one's job), `model` (a subagent's model: `session_id` is the child, the input names the parent, kind and how), `job_approval` (whether the owner asked for a job the agent creates); older rows from the workflow's shadow decisions. | `agent::log_decision` (from `agent`, `score::judge` for `web` and `search`, `wiki`, `workshop`, `delegate`, `jobs`), `memory::sleep` | `delegate::stats` (`model` points) |
| `policies` | 0011 | The routing policy for subagents, versioned (`version`, `data` `{routes: {kind: {model, candidates}}, explore}`, `reason`, `created_by` `owner`, `routes` (the nightly job) or `sleep` (before it)); the latest is in force | `delegate::set_policy` (from `api` and `delegate::tune`), `api::undo_policy` | `delegate`, `api` |
| `memories` | 0015; `tainted` 0022 | Memory entries: text, source (`owner`, `verified`, `inferred`), `tainted` (saved or last replaced by a session that read untrusted content: left out of the instructions, untrusted in search, never promoted to a prompt file), tier (`short`, `long`, `archived`), use counts, the last sleep's scores, `proposed` (`wiki` once a sleep copied it into the wiki; `promote`/`user` from before D-045), why the tier changed (`reason`) | `memory` (`remember`, `sleep`), `search` (`used_at`, `uses` of returned memories) | `memory`, `api`, `search` (indexer) |
| `sleep_runs` | 0015 | One row per sleep (memory only; the nightly upkeep jobs record `job_runs`): trigger (`nightly`, `ceiling`, `owner`), scorer, counts, note, error | `memory::sleep` | `memory` (`morning_note`, `last_run`), `api` |
| `search_docs` | 0017 (extensions `vector`, `pg_trgm`); `embed_attempts`, `embed_error`, index `search_docs_to_embed` 0021 | One searchable document per turn (`kind = 'turn'`, `ref` `<session>:<seq>`), memory (`memory`, `ref` `m<id>`) or wiki page (`wiki`, `ref` = the slug): `ident` (exact names, trigram index), `title`, `body`, generated `tsv` (`simple` config, GIN), `embedding vector(1536)` (HNSW, cosine) + `embed_model`, `at`; `embed_attempts`/`embed_error` (refusals of this content; 3 and it's left to text search) | `search` (indexer) | `search::query` |
| `search_state` | 0017 | The indexer's watermark `tape` (last tape event id); `memories` (a last `updated_at`) and `wiki` are from older releases and no longer read | `search` | `search` |
| `search_queue` | 0021 (with trigger function `zen_queue_memory`) | Memories to index: a row per inserted, deleted or changed (`text`, `tier`, `source`) memory, written by triggers on `memories` in the change's own transaction; the indexer deletes the rows it indexed | triggers on `memories` | `search` (indexer) |
| `searches` | 0017 | Every `search` call: session, query, scope, results (`kind`, `ref`, `exact`, `relevance`), `reranked` | `search` | — (to measure search) |
| `jobs` | 0019 | Scheduled jobs (D-046): `kind` `system` (`action` `sleep`, `wiki`, `skills`, `routes`, `prune`, `engines`) or `agent` (`prompt`, `context`, `skills`, `model`, `workspace`, `report_file` (0023, D-055: the file whose text is an `ok` run's report), `post_to` (0026, D-060: the owner's session an `ok` run's report is posted into)); `schedule` and `tz`; `enabled`, `paused_reason`; `created_by` `owner`/`agent`/`kernel`, `created_in`, `approval` (System One's judgment); scheduler state `next_run_at`, `last_*`, `failures`; `removed_at`. Unique `name` among live jobs | `jobs` | `jobs`, `api` |
| `job_runs` | 0019; `tainted` 0022; `posted_to` 0026 | One row per run: `trigger` (`schedule`, `catchup`, `owner`, `agent`), `status` (`running`, `ok`, `silent`, `error`, `missed`, `interrupted`), `session_id`, report (`output`) or `error`, `tainted` (its session read untrusted content: the report is untrusted wherever it's read), `pending_to` (durable queued destination), `posted_to` (the session the report was posted into, D-060). A partial unique index allows one `running` run per job | `jobs` | `jobs`, `api` |
| `prompt_suggestions` | 0020 | One row per suggested next prompt: `suggested`, `after_seq`, `model`, `prompt_version`, `latency_ms`, `cost_usd`; when the next prompt arrives, `outcome` (`accepted`, `edited`, `declined`, `unseen`) and `final` (what was sent, unless accepted) | `assist` | `assist` (`stats`) |
| `draft_taint` | 0022 | Draft skills (`domain/name`) a session that read untrusted content wrote, with that session and when; out of the agent's reach; the row goes when the owner accepts or rejects the draft | `workshop` (`save_skill`, `mark_draft_write`, `decide_draft`) | `workshop` (`tainted_drafts`: `on_accept`, `stats`), `agent` (`find_skills`/`load_skill`, `read`) |
| `made_tools` | 0018 | One row per tool the agent made: `approved_at`, `approved_sha` (its content when approved), `rejected_at` (the owner's decision, kept out of the tool's folder the agent can write) | `workshop` (`save_tool` insert, `decide_tool`) | `workshop` (`run_made`, `made_tools`) |

`scripts/db.sh pending` lists migrations the live database hasn't applied; `scripts/db.sh backup`
dumps it to `~/.zenbot/backups/` (last 3 kept; the nightly `prune` job keeps the same 3).

---

## Environment variables

The service reads `~/.zenbot/env` (systemd `EnvironmentFile`). `install.sh` writes `ZEN_TOKEN`,
`ZEN_PORT`, `ZEN_REPO`, `ZEN_WORKERS`, `HOME`, `PATH` and `DATABASE_URL` there. Workers inherit
the kernel's environment except its secret-looking variables (`secrets::is_secret_var`: `ZEN_TOKEN`,
`DATABASE_URL`, `OPENROUTER_API_KEY`, `BRAVE_API_KEY`, `TAVILY_API_KEY`, …), the engines' sign-ins
(`ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `OPENAI_API_KEY`, …) kept (`mind.rs`); the agent's shells get none of the kernel's secret-looking variables
(`secrets::is_secret_var`).

### Kernel (`zend`)

`config.rs` reads the kernel's tunable settings once, at start: typed, each default there and
nowhere else, a value that doesn't parse falling back to its default (logged), a flag off with
`0`, `off`, `false` or `no`. Modules read `config::get()`, never the environment. Still read where
they're used (ROADMAP.md, debt): the boot settings in `main.rs`, the paths (`ZEN_HOME`,
`ZEN_*_DIR`, `ZEN_MCP_CONFIG`), the workers' and `ZEN_FAUX`; API keys stay in the environment.
"Used in" names the module a setting steers.

| Var | Default | Used in | Effect |
|---|---|---|---|
| `ZEN_TOKEN` | required | `main.rs` | API token; also masked in tool output |
| `DATABASE_URL` | `postgres://zen:zen@127.0.0.1:5432/zen` (the bootstrap password: CI, throwaway setups) | `main.rs`, `scripts/db.sh` | Database. `install.sh` and upgrades write it to `env` with the private password from `~/.zenbot/db-password` (`db_secure`); the scripts' throwaway databases use the same user and password (`db_url_for`) |
| `ZEN_PORT` | `8100` | `main.rs`, scripts | Listen port |
| `ZEN_BIND` | `127.0.0.1` | `main.rs` | Listen address; opt in to external access explicitly |
| `ZEN_WORKSPACE` | `$HOME` | `main.rs` | Default working directory for tools |
| `ZEN_REPO` | `$HOME/zenbot` | `main.rs`, `zen` CLI | zenbot's checkout: named in the system prompt; used by the updater; its AGENTS.md files are trusted instructions (`context.rs`) |
| `ZEN_TRUSTED_REPOS` | (none) | `context.rs` | Comma-separated repository paths whose AGENTS.md / CLAUDE.md are attached as instructions when a tool touches them (besides `ZEN_REPO`; git worktrees of these count too); any other project's file comes as untrusted content and taints the session (D-051) |
| `ZEN_DEFAULT_MODEL` | `claude/claude-opus-5-5` | `main.rs` | Model for new sessions |
| `ZEN_HARNESS` | `~/.zenbot/version` | `main.rs` | Build id recorded with every turn (dev, smoke and eval kernels set it) |
| `ZEN_WORKERS` | `engine` | `workers.rs`, scripts | Workers to start; a name other than `engine` without its `ZEN_WORKER_<NAME>_CMD` is ignored (warning, `/health` `ignored_workers`) |
| `ZEN_ENGINE_CMD` | `zen-engine` next to `zend` | `workers.rs` | Command for `engine` |
| `ZEN_WORKER_<NAME>_CMD` | (none) | `workers.rs` | Command for any other worker name (required) |
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
| `ZEN_POLICY_MIN_JUDGED` | `20` | `delegate.rs`, `api.rs` | Judged subtasks each model needs before the nightly `routes` job may switch a route |
| `ZEN_JOBS` | on | `jobs.rs` | `0`: the scheduler doesn't run (smoke, e2e, eval and dev kernels set it); system jobs are still seeded |
| `ZEN_JOB_BAR` | `0.9` | `jobs.rs` | System One's probability that the owner asked for a job the agent creates, needed for it to go live |
| `ZEN_TZ` | `America/Sao_Paulo` (the owner's zone) | `jobs.rs` | Default timezone of a new agent job (an IANA name) |
| `ZEN_JOBS_TICK` | `60` | `jobs.rs` | Longest the scheduler sleeps between checks, in seconds (tests make it shorter) |
| `ZEN_S1_MODEL` | unset (off) | `score.rs` | OpenRouter System One classifier: scoring, `decide`, the sleep, reranking, `capture`, delegation, the job gate |
| `ZEN_S1_URL` | `https://openrouter.ai/api/v1/systemone` | `score.rs` | System One endpoint override (primarily for local tests) |
| `ZEN_S1_PRIVATE` | on | `score.rs` | `0`: no private content goes to OpenRouter: no embeddings, no System One reranking or `focus`, no `decide` tool, no sleep judgments, `capture`/`skill_duplicate`/the subagent's kind decided without it (callers of `score::private_ok`) |
| `ZEN_SEARCH_PROVIDER` | `brave` if `BRAVE_API_KEY` is set, else `tavily` if `TAVILY_API_KEY` is, else `searxng` | `web.rs` | Which search provider `web_search` uses |
| `OPENROUTER_API_KEY` | unset | `score.rs`, `search.rs` | Authenticates System One and search embeddings at OpenRouter (kept from workers and shells) |
| `BRAVE_API_KEY` / `TAVILY_API_KEY` | unset | `web.rs` | Keys for Brave / Tavily search (secret-looking, so masked in tool output) |
| `ZEN_SEARXNG_URL` | `http://127.0.0.1:8888` | `web.rs` | SearXNG base URL: the keyless provider and the rescue for a failed keyed call |
| `ZEN_SEARCH_RERANK` | on | `web.rs` | `0`: don't have System One rerank search results |
| `ZEN_MCP_CONFIG` | `$ZEN_HOME/mcp.json` | `mcp.rs` | MCP servers file; `${VAR}` in it filled from the kernel's environment (a secret-named one only for a server that lists it in `allow_env`) |
| `ZEN_SCORE_IDLE_SECS` | `7200` | `score.rs` | Idle time before a session is scored (longest-waiting first, 5 a minute; a failing one retried after 1, 2, 4 … 128 hours) |
| `ZEN_CONTEXT_TOKENS` | `200000` | `compact.rs` | Context budget (capped by the model's window) |
| `ZEN_COMPACT_SOFT` / `ZEN_COMPACT_HARD` / `ZEN_COMPACT_KEEP` | `0.7` / `0.9` / `0.3` | `compact.rs` | Fractions of the budget: prepare a summary / apply now / keep verbatim |
| `ZEN_COMPACT_IDLE_SECS` | `300` | `compact.rs` | Pause after which a prepared summary is applied |
| `ZEN_ASSIST_MODEL` | `claude/claude-haiku-5-5` | `assist.rs` | Writes session names and suggested next prompts (the cheapest, fastest model on the owner's plans) |
| `ZEN_SUGGEST` | on | `assist.rs` | `off` (or 0/false/no): no suggested next prompts (names stay on) |
| `ZEN_SUMMARY_MODEL` | `claude/claude-sonnet-5-5` | `compact.rs` | Summarizer |
| `ZEN_CACHE_TTL` | `claude=3600` | `measure.rs` | How long each engine's prompt cache lives, in seconds, as `engine=secs` pairs (`*=secs` for the rest; 300 for an engine not named): a turn after a longer pause is measured as a cache miss |
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
| `ZEN_FAUX` | off | `faux.rs` | `1` lists the scripted models `faux/smoke` and `faux/backup` |
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
| `ZEN_DEV_REAL` | off | `dev.sh` | `1` keeps the service's paid keys and folder settings (workspace, wiki, skills, tools, MCP config) in the dev kernel |
| `ZEN_E2E_PORT` / `ZEN_E2E_JOBS` / `ZEN_E2E_KEEP` / `ZEN_E2E_NO_BUILD` / `ZEN_E2E_BIN` | `18377` / `4` / – / – / `target/release` | `e2e.sh` | base port (scenario n uses base + 20 × n, its test servers the next ports); scenarios run at once; keep DBs and files; skip the build (CI); the binaries to test (CI's release job: `target/dist`) |
| `ZEN_E2E_MCP_TOKEN` | set by `e2e.sh` | `e2e.sh` (`mcp` scenario) | Tests `${VAR}` expansion in `mcp.json` headers (listed in the server's `allow_env`) |
| `ZEN_MATRIX_E2E_PORT` / `ZEN_MATRIX_E2E_HS_PORT` | `18197` / `16167` | `matrix-e2e.sh` | Faux kernel port; throwaway homeserver port |
| `ZEN_SLOW_SECS` | `12` | `scripts/e2e/slow_worker.py` | How long the test summarizer's `complete` takes |
| `ZEN_EVAL_PORT` / `ZEN_EVAL_KEEP_BUILDS` | `18301` / `5` | `eval.sh` | First eval kernel port (runs at once use the next ones); base builds kept |
| `ZEN_EVAL_JOBS` / `ZEN_EVAL_CACHE_DAYS` | `3` / `7` | `eval.sh` | Eval runs at once (`--jobs`); days a cached base result is reused |
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
| `env` | `install.sh`, `apply-upgrade.sh` (`DATABASE_URL`, a retired worker out of `ZEN_WORKERS`) | systemd (`EnvironmentFile`), `zen` CLI, `zen-matrix` (`ZEN_PORT`), scripts (`lib.sh::zen_env`) | Service environment; mode 600; hidden from sandboxed shells |
| `env.prev` | `lib.sh::env_backup` (`install.sh`, `apply-upgrade.sh`, `db.sh secure`) | owner | **Secret.** `env` as it was before a script last rewrote it; mode 600; hidden from sandboxed shells |
| `db-password` | `db.sh::db_secure` (`install.sh`, `apply-upgrade.sh`) | `db_secure` (checks `DATABASE_URL` carries it), `secrets.rs` (masked) | **Secret.** The database user's password (mode 600), replacing the bootstrap `zen`; hidden from sandboxed shells |
| `token` | `lib.sh::new_token` (install, dev) | `zen` CLI, `install.sh` (copies into `env`), `secrets.rs`, `zen-matrix`, scripts | **Secret.** API token; hidden from sandboxed shells |
| `version` | `install.sh`, `apply-upgrade.sh` | `update.rs`, `zen` banner | Installed commit |
| `upgrade.log` | `apply-upgrade.sh`, `update-engines.sh` | owner, `update.rs` (last line), `zen upgrade` | Upgrade and engine-update results |
| `upgrades/` | `upgrade.sh` (one stage per request: the tested `zend`, `zen`, `zen-engine`, `commit`, `note`; stages over 2 hours old removed), `apply-upgrade.sh` (`.lock`, `.installed`; removes its stage) | `apply-upgrade.sh` | Builds waiting to install; the newest request wins |
| `engines.json` | `update-engines.sh` | `zen status` | Engine versions from the last check |
| `engines.lock` | `update-engines.sh` | `update-engines.sh` | `flock` so only one engine update runs at a time |
| `history` | `zen` TUI | `zen` TUI | Prompt history (JSON lines, mode 600) |
| `backups/` | `db.sh backup` (before pending migrations; last 3 dumps kept, also by the nightly `prune` job) | owner | Database dumps (`<UTC time>-<label>.dump`) |
| `backups/prompt-files/` | `memory::backup_file`: before the sleep promotes into or compacts a prompt file, and before an agent's `edit`/`write` to one (`dispatch`) | owner | Copies of `SOUL`/`IDENTITY`/`AGENTS`/`USER` (D-045); the nightly `prune` job removes those older than 90 days |
| `bin/` | `install.sh`, `apply-upgrade.sh`; `scripts/matrix.sh` (`zen-matrix`) | systemd, `~/.local/bin/zen` link | `zend`, `zen`, `zen-engine` (+ `.prev` for rollback), `zen-matrix` |
| `outputs/` | `tools.rs`, `web.rs`, `mcp.rs` | the model (`read`, `bash`); the nightly `prune` job removes these files after 30 days | Full output of cut tool results; PDFs from `web_fetch` (`web-*.pdf`); MCP output over 50 KB (`mcp-*.txt`). All three use `outputs_dir()` (`<zen home>/outputs`, mode 700) |
| `engine/claude/`, `engine/complete/`, `engine/codex/`, `engine/sockets/` | `zen-engine` (`turn::engine_dir`, mode 700) | Claude Code, Codex | Fixed directories for Claude Code sessions and completions, Codex's empty working dir and private `CODEX_HOME`, per-turn tool sockets |
| `AGENTS.md` | `defaults.rs` when missing, then the owner | `compile.rs` | System-wide prompt file: the environment |
| `USER.md` | `defaults.rs` when missing, then the owner, the agent and the sleep (`## Learned`; backed up on every change) | `compile.rs` | System-wide prompt file: the owner (D-045) |
| `agents/zenbot/SOUL.md` | `defaults.rs` when missing, then the owner | `compile.rs` | The agent's own prompt file: who it is (`layout::SOUL`; one agent today) |
| `agents/zenbot/IDENTITY.md` | `defaults.rs` when missing, then the agent and the sleep (backed up on every change) | `compile.rs` | The agent's character and how it works, after the soul (D-045); `edit`/`write` on it (and SOUL, AGENTS, USER) refused for subagents and tainted sessions (`agent::protected_refusal`) |
| `global/MEMORY.md` | `memory::export` | the owner | Copy of short-term memory (edit with `remember`, not here) |
| `global/skills/<domain>/<name>/` | `defaults.rs` (`work/brief`, `work/verify`, `work/close`) when missing, then `save_skill` (and the agent's file tools: active skills only from an untainted owner's session, drafts from any), the nightly `skills` job (archiving) | `skills.rs`, `workshop.rs` | Skills; a git repository once the workshop first commits. `_proposed/<domain>/<name>`: drafts; `_archived/…`: rejected or unused skills |
| `global/tools/<name>/` | `save_tool` | `workshop.rs` (catalog, `run_made`) | The agent's own tools: `tool.json` and files; a git repository (`ZEN_TOOLS_DIR`). Approval lives in `made_tools`, not here; each run uses a temporary copy, so what a tool writes never lands here |
| `global/wiki/` | `wiki.rs` (`capture`: pages, `index.md`, `log.md`, git commits), the agent (`edit` of summaries), the nightly `wiki` job (commits) | `search.rs` (indexer), the model (`read`), the owner | The wiki (`ZEN_WIKI_DIR`), its own git repository |
| `mcp.json` | the owner (never the agent's `edit`/`write`) | `mcp.rs` (re-read when it changes) | MCP servers (`mcpServers`); keep secrets in `env` and refer to them as `${VAR}`, listed in the server's `allow_env` when secret-named |
| `SOUL.md`, `MEMORY.md`, `wiki`, `skills`, `tools` (symlinks) | `layout::migrate` | a rolled-back build | The old flat layout's paths, each a relative symlink to its new place (D-040); removed in a later release |
| `evals/<run>/` | `eval.sh` (the nightly `prune` job removes runs older than 7 days) | `eval-report.sh` | Eval results: `base.jsonl`, `new.jsonl`, `selection.txt` (which tasks and why), `report.md` |
| `evals/cache/<key>.json` | `eval.sh` | `eval.sh` | Base results reused while young and unchanged (key: base commit, model, effort, the task's files, engine versions, repeat) |
| `evals/builds/<commit>/` | `eval.sh` | `eval.sh` | Base harness builds (the newest `ZEN_EVAL_KEEP_BUILDS`) |
| `matrix.env` | the owner | `zen-matrix` | **Secret** (mode 600): `MATRIX_USER`, `MATRIX_OWNER`, `MATRIX_PASSWORD` (first sign-in only), optional `MATRIX_RECOVERY_KEY`, `MATRIX_HOMESERVER` |
| `matrix/` | `zen-matrix` | `zen-matrix` | Mode 700. **Secrets:** `session.json` (access token), `store.key` (encrypts `store/`, the SQLite key and sync store), `recovery-key`. `state.json`: rooms ↔ sessions, per-room `seen` seq, the owner's pinned master key, job-report cursor. `run.lock`: held while the service runs (`login`, `repin-owner` refuse next to it) |

Claude Code and Codex keep their own sign-ins in `~/.claude` and `~/.codex`.

---

## Scripts, install, deploy, CI

| Path | Does | Touches |
|---|---|---|
| `install.sh` | Fresh-VM install (safe to re-run): apt packages (git, curl, ca-certificates, jq, bubblewrap, poppler-utils, docker, docker compose), Node 22 in `~/.local/node` (pinned tarball, SHA-256 checked), Claude Code and Codex CLIs, binaries (download or build), `~/.zenbot/{bin,env,token,version}`, a private database password (`db_secure`), the `zenbot` systemd unit (and `remove_old_timers`), git hooks path. Doesn't install `zen-matrix` (`scripts/matrix.sh`) | system, `~/.zenbot`, `/etc/systemd/system` |
| `scripts/upgrade.sh` | Build (or fetch) and check, smoke-test on a second kernel (`:18199`, a throwaway copy of the live DB), stage the tested binaries in `~/.zenbot/upgrades/<id>` and schedule `apply-upgrade.sh` via `systemd-run`; `--check` stops before staging. Every step: DEVELOPMENT.md, "Applying a change" | `target/`, temp DB `zen_smoke_*`, `~/.zenbot/upgrades` |
| `scripts/apply-upgrade.sh` | Detached: waits for idle, backs up the DB if migrations are pending, swaps in the staged `zend`, `zen`, `zen-engine` (keeps `.prev`), brings `env` up to date (`db_secure`, retired `pi` out of `ZEN_WORKERS`), restarts, rolls back if unhealthy; when healthy, `remove_old_timers`, `docker compose … up -d`, then `trim_target` on the `ZEN_REPO` checkout's top-level `target/` and the one under `crates/zen-matrix`, and `docker_prune`. Every step: DEVELOPMENT.md | `~/.zenbot/{bin,version,upgrade.log,backups,upgrades}`, service |
| `scripts/self-update.sh` | `git pull` main then `upgrade.sh`; refuses a dirty checkout or another branch. Used by `zen upgrade`, `/upgrade`, `POST /api/upgrade` | checkout |
| `scripts/fetch-release.sh` | Download CI's binaries for HEAD into `target/release` (checksum verified); fails (changing nothing) on local changes under `crates/`, `Cargo.*`, non-x86_64-Linux, or no build. `--check REF` only checks | `target/release` |
| `scripts/update-engines.sh` | Daily (the kernel's `engines` job): update Claude Code / Codex CLIs, test with a real `complete` through `zen-engine` (and `zen-engine codex-features` for Codex), roll back on failure; the Codex feature check also runs daily on its own (exit 1 on failure); `--check` reports only | CLI installs, `upgrade.log`, `engines.json` |
| `scripts/db.sh` | DB helpers run inside the Postgres container (`docker`, else `sudo -n docker`): `pending` (fails when it can't ask), `backup`, `secure` (`db_secure`: the private password), copy/drop/restore helpers | live DB, `~/.zenbot/backups` |
| `scripts/lib.sh` | `zen_env`, `env_set`, `env_backup`, `wait_healthy`, `ensure_rust`, `new_token`, `remove_old_timers` (stops and deletes the `zen-sleep`/`zen-engines` timers older versions installed; `install.sh` and, after a healthy upgrade, `apply-upgrade.sh`), `trim_target` (a cargo target dir over 2 GiB loses its incremental caches and `debug/`, then all of it if still over; skipped while cargo runs) and `docker_prune` (dangling images, build cache unused for 168h), both after a healthy upgrade | `/etc/systemd/system`, `target/` dirs, Docker |
| `scripts/dev.sh` | Dev kernel in the foreground on `:18100` with database `zen_dev` and `ZEN_HOME` `~/.zenbot-dev`, using `~/.zenbot/env` settings without the paid keys and with the dev home's folders (workspace `~/.zenbot-dev/workspace`) unless `ZEN_DEV_REAL=1` | `zen_dev` DB, `~/.zenbot-dev` |
| `scripts/e2e.sh` | End-to-end scenarios (Tests, below); scripts in `scripts/e2e/*.json`, plus test servers in Python: `slow_worker.py` (a worker serving `slow/summarizer`, a deliberately slow `complete`; started as `ZEN_WORKER_SLOW_CMD`, and as `ZEN_WORKER_ENVDUMP_CMD` to write a worker's environment), `mcp_server.py` (an MCP server with `echo` and `add`, stdio or `--http PORT`), `searxng_stub.py` (answers `/search?format=json`), `systemone_stub.py` (validates the direct classifier call), `embed_stub.py` (an `/embeddings` stand-in; `search_reference.sql` beside it is the previous search query, to compare rankings), `ws_prompt.py` (sends one WebSocket message as zen's terminal app does and prints the events) | temp DB, workspace and `HOME` |
| `scripts/check.sh [filter]` | What CI checks (CI's `check` job runs it): `scripts/check-docs.py --base origin/<base>`, build once, unit tests, clippy `-D warnings`, `scripts/size-budget.sh`, `scripts/e2e.sh [filter]` on that build; times each step, stops at the first failure | `target/`, e2e's temp DBs |
| `scripts/size-budget.sh [dir]` | Fails when `zend`, `zen` or `zen-engine` in `target/release` (or `target/dist`) is over its size budget (measured size + ~15%, per profile) | – |
| `scripts/eval.sh`, `scripts/eval-report.sh` | Harness eval: this checkout vs installed (or `--base REF`), same model, on the tasks the change affects (`evals/areas.txt`; `--plan` shows which and why, `--full` all). Runs `--jobs` at once, each with its own kernel, port, database and `ZEN_HOME` next to the task workspace (default prompt files and skills); base results cached. `--native claude\|codex` makes the base the vendor's CLI. Report for the owner, never a gate | `zen_eval_*` DBs, `~/.zenbot/evals/` |
| `scripts/eval-native.sh` | One eval turn through the vendor's own CLI (Claude Code or Codex, with their own tools), printed like `zen ask --json`: the base of `eval.sh --native` | the task workspace |
| `scripts/eval-paired.py` | Paired comparison of eval runs: tasks only one side passed with a sign test, cost, token, time and tool-call ratios with bootstrap intervals | – |
| `evals/areas.txt` | Path prefix → areas, so `eval.sh` runs only the tasks a change can affect (D-050) | – |
| `evals/lib/coding-zen-insert.py` | Puts a hidden test into a Rust file's `mod tests` (the `coding-zen-*` checks) | the task workspace |
| `scripts/git-hooks/prepare-commit-msg` | Adds `Zen-Session` / `Co-Authored-By` trailers when `ZEN_SESSION_ID` is set | commit messages |
| `deploy/compose.yaml` | `postgres` (pgvector, `127.0.0.1:5432`; its password `zen` is only the bootstrap one, `db_secure` replaces it) and `searxng` (pinned by digest, `127.0.0.1:8888`, keyless search for `web_search`) | Docker volume `zen-pg` |
| `deploy/searxng/settings.yml` | SearXNG settings mounted read-only: JSON output on, limiter and image proxy off, not a public instance; a fixed, public `secret_key` (it protects nothing on 127.0.0.1 with the image proxy off) | — |
| `scripts/matrix.sh` | Build and test `crates/zen-matrix`, install `~/.zenbot/bin/zen-matrix`, `zen-matrix login` if not signed in, install and restart `zen-matrix.service`. `--build` stops after the tests | `~/.zenbot/{bin,matrix}`, `/etc/systemd/system` |
| `scripts/check-docs.py` | The docs still match the code: MAP.md names every route, tool, setting, table, e2e scenario and crate; paths, settings and `D-NNN` named in current docs exist; the docs stay within their line budgets; with `--base <ref>`, a change to the code adds a PROGRESS.md entry. CI job `docs` | – |
| `scripts/matrix-e2e.sh` | The Matrix channel end to end: a throwaway continuwuity homeserver in Docker, a faux kernel (temp DB, own port and home), the bridge and the scripted owner (`examples/owner.rs`, cross-signed) through E2EE; also refusals (plaintext, an unverified session, an owner that is the bot), an answer posted after a bridge restart, a stranger's invite and an unencrypted room declined, a room left when a stranger is invited, and a job report delivered | Docker container, temp DB |
| `deploy/zen-matrix.service` | Runs `~/.zenbot/bin/zen-matrix run` once `~/.zenbot/matrix/session.json` exists; home is an empty tmpfs with only its binary, `token`, `env`, `matrix.env` (read-only) and `matrix/` (read-write) bound in; `ProtectSystem=strict`, no devices, capabilities or new privileges, IP and Unix sockets only, `@system-service` system calls | — |
| `deploy/zenbot.service` | Runs `~/.zenbot/bin/zend` with `EnvironmentFile=~/.zenbot/env`; `ExecStartPre` brings Postgres up (must succeed) and SearXNG (a failure is ignored); `Restart=always` | — |
| `.github/workflows/ci.yml` | On PRs and pushes to main: job `check` runs `scripts/check.sh`; job `matrix` builds, tests and lints `crates/zen-matrix`. Actions pinned by commit SHA. On main, if `check` is green: build `dist` profile, check its size budgets, run the e2e scenarios on it, publish `zenbot-x86_64-linux-<sha12>.tar.gz` to the rolling `edge` release (20 newest kept) | GitHub releases |

---

## Tests

- **Unit tests** live in `#[cfg(test)] mod tests` at the bottom of each file, in every crate
  (`zen`'s `tui/` render and key tests use `tui/test_util.rs`). Run `cargo test --release`; for
  `zen-matrix`, `scripts/matrix.sh --build` (its own workspace).
- **End to end:** `scripts/e2e.sh [filter]` (how it runs and how to add a scenario: DEVELOPMENT.md,
  "End-to-end scenarios"). What each scenario checks is the comment above its function in
  `scripts/e2e.sh`; the faux scripts are `scripts/e2e/*.json` and the test servers
  `scripts/e2e/*.py`. Scenarios (each runs its parts, the functions after its name):
  `turns`, `jobs`, `search-index`, `taint-on-data`, `summaries`, `system-one`, `action-gate`,
  `file-trust`, `reach`, `basics`, `memory`, `delegation`, `names-suggestions`, `trust`,
  `workshop`, `verifier`, `agent-tools`, `search`, `wiki`, `engine-boundary`, `failover-model`.
- **Evals:** `evals/tasks/<name>/{task.json,files/,hidden/}` (23 tasks: 13 on how zenbot works,
  9 `coding-*` with hidden tests, and the runner self-test `smoke`), run by `scripts/eval.sh` on the
  tasks a change affects (`evals/areas.txt`, plus the `core` tasks); see `evals/README.md`. Harness
  changes (system prompt, history, tools, workers, model or effort handling) get an eval before
  merging; the report goes to the owner, who decides.
- **Upgrade smoke test:** one scripted `faux/smoke` turn inside `scripts/upgrade.sh`.
- **Matrix channel:** `scripts/matrix-e2e.sh` end to end (Scripts table); not run by `scripts/e2e.sh` or CI.

---

## Notes and known gaps

- **Columns and tables of the old workflow stay** (expand-only): `sessions.state` and old tape
  blocks. Nothing writes `state` any more and the API no longer returns it; `compile::history`
  still honours an old `state` block with `fresh: true` (history starts after it), so old sessions
  replay as before. Drop them in a later release.
- **Criteria commands run in the session's shell sandbox**, not the verifier's. `verify` runs a
  criterion's `run` command on the bash tool's core (`tools::run_shell`: own process group, 600 s
  timeout, output masked) in the sandbox the session's own `bash` gets: none for a clean session
  (able to write), offline/read-only for a risky one unless the exact verification is granted, even though the verifier's own shell is read-only.
- **`GET /api/mcp` connects every configured server** (starts stdio ones) to count their tools.
  No client calls it (not `zen status`).
- **The skill rules apply only through `save_skill`.** An untainted owner's session can change an
  active skill or tool file directly with `write`/`edit`, and any session can write a draft (a
  tainted, child or kernel session is refused the active ones: `agent::protected_refusal`); `bash`
  can change any of them (including activating a draft by moving it); the next workshop commit
  records it. Changes to an active skill through `save_skill` apply at once, without review.
- **The duplicate check covers only new skills in the same domain**, against active skills and
  drafts; System One's judgment is used only with private content allowed, else word overlap.
- **`bash` can still write the protected files** (prompt files, active skills, made tools, the
  owner's config). `protected_refusal` covers `edit`/`write` only; a shell command runs as the
  owner's Unix user (ROADMAP.md, debt).
- **Agent edits to the wiki are committed only by the next capture or `wiki` job**, together, as one
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
  from a session that read web content are wrapped as untrusted and taint the recalling session
  (`history`, or `search` asked for it by name); a search that only comes across one lists it
  without its text. A changed `ZEN_EMBED_MODEL` re-embeds in the background; vector search
  compares only rows of the current model. The indexer takes tape events once they are 5 seconds
  old, so an id committed late isn't skipped.
- **Wiki and untrusted content:** search results from a wiki page holding entries labelled `web`
  are wrapped as untrusted and taint the session; a page read directly with `read` isn't. The wiki
  indexer compares file times in milliseconds.
- **Made-tool approval covers the content** (a SHA-256 of the manifest and files, computed in
  Postgres, over the private copy that runs): a tool changed after approval runs sandboxed again;
  the owner approves by the fingerprint they reviewed. Unapproved tools are offline and
  read-only, with an empty home, but can read the rest of the filesystem (`--ro-bind / /`); approved
  ones run with `bash -c` (not a login shell) and full write access.

## Deeper docs

Every doc and what it holds: [README.md](README.md#docs).
