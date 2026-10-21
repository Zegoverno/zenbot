# zenbot — Design

> How the system is built and how it works: first as it is today, then the target design agreed on
> 2026-10-06 (D-025 to D-030) that the roadmap builds. What zenbot is meant to have, module by module,
> is in [SPEC.md](SPEC.md); where each piece lives in the code, in [MAP.md](MAP.md); the order of
> work, in [ROADMAP.md](ROADMAP.md). Deep dives: `docs/context.md` (what the model reads each turn),
> `docs/brief.md` (briefs and verification), `docs/worker-protocol.md`,
> `docs/client-protocol.md`, `docs/matrix.md`.

## Invariants

These hold today and in the target design. Changing one needs the owner's OK and a DECISIONS.md entry.

1. **The kernel owns all state and runs every tool call.** Workers are stateless: they get a turn's
   context and ask the kernel to run tools. Engines run with their own tools off (Claude Code
   `--tools ""`, Codex's tool features disabled), and a run that shows or uses one is stopped, so
   every action goes through the kernel.
2. **The tape is the source of truth.** The model reads only what the tape contains, and the tape
   contains everything the model read. Engine sessions are a disposable cache rebuilt from it.
3. **The prefix is fixed per session.** Instructions and tools are stored once per session (an
   envelope); per-turn context goes at the end; history is only appended to. This keeps the
   provider's prompt cache.
4. **Every turn is measured:** what produced it (build, engine and version, model, effort), what was
   sent, cache breaks, tokens, cost, time, tool errors.
5. **The owner's verdict is the ground truth.** Evals compare harness versions and inform the owner;
   they never gate.
6. **Secrets never reach the model, the tape or logs**: masked in tool output; full outputs kept
   privately.

---

## As built

### Processes

```
 zen (terminal app, script commands)   zen-matrix (Matrix channel)
          │ HTTP + WebSocket, one port (ZEN_PORT, default 8100, loopback), owner token
┌──────────────────────── zend (Rust kernel, systemd service `zenbot`) ────────────────────────┐
│ API/WS · sessions and tape · context compiler · tools and executor · memory and sleep · skills │
│ System One decisions (S1) · summaries · tracing · worker routing and supervision               │
└────────────┬───────────────────────────────┬─────────────────────────────────────┘
             │ JSON-RPC 2.0 over stdio        │ sqlx / HTTPS for System One
    zen-engine (Rust)                    Postgres + pgvector / OpenRouter
    Claude Code CLI ─ MCP bridge ─┐      (Docker, deploy/compose.yaml)
    Codex app-server ─ dynamic    │
    tools                         └── tools come back to zend
```

| Process | Language | Responsibility |
|---|---|---|
| `zend` | Rust (axum, sqlx, tokio) | Always-on kernel; owns state and side effects; single binary |
| `zen` | Rust | Terminal app (full screen with a sessions board as home (`GET /api/board`, D-048), diffed frames, scrolling and a file side panel, or `--inline`) and script commands (`--json`) |
| `zen-matrix` | Rust (matrix-sdk) | Optional Matrix channel (D-049, `docs/matrix.md`): a client of the client protocol like `zen`, one encrypted room per session plus a "zen jobs" room for job reports; systemd service `zen-matrix`, set up by `scripts/matrix.sh` |
| `zen-engine` | Rust | Default worker: runs turns on the Claude Code CLI and `codex app-server` on the owner's subscriptions, with zenbot's prompt, tools and history |
| Postgres | — | 16+ with pgvector; local in compose, movable via `DATABASE_URL` |

The kernel starts each worker as a child process, supervises and restarts it, ends orphaned turns,
and stops stalled turns (watchdog). Each worker lists the models it serves (`models.list`), and the
kernel routes a model to the worker that lists it (`claude/…` and `codex/…` to zen-engine). The protocol is in `docs/worker-protocol.md`; the client protocol in
`docs/client-protocol.md`. A scripted model, `faux/smoke` (`ZEN_FAUX=1`), runs turns without a
subscription for tests. By default `/api/models` shows every model reported by the workers;
`ZEN_MODELS` can explicitly curate the visible list. `zen-engine` includes Haiku 5.5 and reads
Codex's live model catalog.

A hard subscription usage cap may trigger one cross-provider continuation in the same kernel turn
(D-044, `docs/worker-protocol.md`). It replays the original request and completed current-turn
messages to the other engine, with no engine-session resume, but stops if a tool's result or side
effect is uncertain. Both providers must be signed in on subscriptions, not API keys. The switch
is shown to the owner and recorded on the tape.

### Sessions, tape and context

Each session has an append-only tape: a chain of blocks with a per-session number (`seq`), a parent
link and a hash over the parent's hash and the content. Block kinds include `message`, `context`,
`envelope`, `compaction`, `engine_session`, `base` (the session's instructions), `questions`,
`verification`, `verdict`, `taint` and `failover` (and, from sessions before 2026-10-07, the old workflow's `state`, `brief`,
`submission`, … blocks).

What the model reads each turn (`docs/context.md`): the envelope (system prompt and tools, fixed for
the session) → a summary of older turns, if any → the history (append-only) → the new message with
its turn context (the date) at the end, sent only when it
changed. The system prompt is the prompt files (laid out by scope, `layout.rs`, D-040) `~/.zenbot/agents/zenbot/SOUL.md`, then `agents/zenbot/IDENTITY.md`, `AGENTS.md` (the environment)
and `USER.md`, short-term memory as of the session's start, the skills index, then `AGENTS.md` (or
`CLAUDE.md`) from `/` down to the workspace; a project's file found later by a tool is attached to
that tool result and kept: as instructions when the project is a trusted repository (zenbot's
checkout, `ZEN_TRUSTED_REPOS`, or a git worktree of one, D-056), otherwise as untrusted content that taints the session (D-051). The kernel writes missing default prompt files and skills at start
(`crates/zend/defaults/`), never overwriting. Each zenbot session keeps a matching Claude Code session (`--resume`) or Codex thread, so
earlier turns come from the provider's cache. Past 70% of the context budget, older turns are
summarized in the background with block addresses; the `history` tool reads any block back.

### Session names and suggested prompts

After an owner's turn ends cleanly, the kernel makes one call to the assist model (`assist.rs`,
D-047; Haiku 5.5 by default, through the worker's tool-less `complete`), reading the session's first
prompt and its latest exchanges (no tool output). It names the session while the title isn't the
owner's (after each of the first 3 turns) and suggests the owner's likely next prompt, or nothing
when there's no clear next step; not after `ask`, nor in jobs or child sessions. The client shows
the suggestion in grey in the empty input (tab takes it). The next prompt reports what the owner did
with it, and `prompt_suggestions` keeps the suggestion, the outcome (accepted, edited, declined,
unseen), what was sent and the prompt version, so the prompt can be improved from evidence.

### Tools today

One fixed list, in a fixed order, the same every turn of a session (`agent::specs`); which tools
each kind of session gets and which module runs each: MAP.md, "Tools the model gets". Each
description says what the tool does, when to use it and when not, and what it returns. Tool output is cut once, when the tool runs (full output saved and referenced); edits
are serialized per file and CRLF/BOM-safe; tools are cancelled with their process group on abort.

### Briefs and verification

A skill and a tool, not a kernel workflow (D-026, `docs/brief.md`): the `work/brief` skill frames a
big, risky or unclear job; the `verify` tool has the kernel run the criteria's commands and, for
criteria that need judgment, a fresh read-only verifier session judge the diff. `ask` ends the turn
with questions for the owner. The kernel-enforced workflow (`flow.rs`) was removed on 2026-10-07.

### Scheduled jobs

One scheduler in the kernel (`jobs.rs`, D-046), backed by `jobs` and `job_runs`:

- **System jobs**, seeded at start (an install from before a job existed gets it at its next
  start), each with its own run record: `sleep` (03:00 UTC, `memory::sleep`), then the nightly
  upkeep ten minutes apart: `wiki` (03:10, `wiki::tend`: commit the agent's own wiki edits, report
  pages without a summary and broken links), `skills` (03:20, `workshop::tend`: flag skills unused
  for 30 days, archive them at 90), `routes` (03:30, `delegate::tune`: switch a delegation route on
  clear evidence) and `prune` (03:40: saved tool outputs after 30 days, prompt-file backups after
  90, database dumps but the newest 3, eval runs after 7 days); and `engines` (04:00 UTC, `scripts/update-engines.sh` with the kernel's secrets removed). An
  upkeep run that found nothing is `silent`; what they found in the last day joins the sleep's
  morning note in the next sessions' instructions (`jobs::upkeep_note`). The owner can pause or
  run them; a system job whose name an agent job already holds isn't seeded (logged).
- **Agent jobs**: a prompt run in a fresh session of kind `job`, titled `job: <name>` and listed
  with the owner's sessions. Its instructions (a `base` block written before the turn) are the
  parts the job picked (`context`: the soul always, plus identity, agents, user, memory, skills,
  project; default user and memory) with its named skills loaded and a `<scheduled_job>` note: no
  one is there, report what needs attention, `[SILENT]` when nothing does, `[FAILED]` when it
  can't, no outward action. The prompt carries the previous report; a run whose session read
  untrusted content has its report marked (`job_runs.tainted`), so the next run gets it as
  untrusted content and is tainted from its start, and `schedule runs` shows it wrapped (D-053). A job session can't `ask`,
  `delegate` or `schedule` (`agent::refusal`). When a run ends, however it ends (ok, silent, error,
  interrupted by a restart), its session is archived (D-057): the board shows a job's session only
  while it runs, and the report reaches the owner through the jobs room and `zen jobs runs`.
- **Report files** (D-055): a job with `report_file` (a path inside its workspace, stored
  absolute) reports what its run wrote there, not the final answer, when the run ends `ok`. The
  file must be a regular file opened without following a symlink, really inside the workspace
  (read back from `/proc/self/fd`), not a secret file, modified after the run started, UTF-8 and
  at most 60 KB; otherwise the run is an `error` naming why. An empty file makes the run
  `silent`. The workspace, not `/tmp`, because a tainted run's shell has a private `/tmp`.
- **Posting into a session** (D-060): a job with `post_to` (one of the owner's own sessions) has a
  clean `ok` run's report appended to that session as an assistant message, once no turn is
  running there (under the turns lock; up to 2 hours), and emitted to its clients, so the Matrix
  room of that session shows it and the next turn sees it (a new message ends engine-session
  resume). `job_runs.pending_to` makes the queue durable across restarts; appending to the tape
  and setting `posted_to` commit atomically. The bridge leaves queued and posted runs out of
  **zen jobs**; an expired queue entry falls back to the feed. A run that read untrusted content
  reports to the jobs feed only.
- **Creating jobs**: the owner (`zen jobs add`, `POST /api/jobs`) or the agent (`schedule`). An
  agent's job goes live only when System One, reading the owner's own messages in the session,
  judges the owner asked for it (decision file `job_approval`, live from 0.9 by its costs; the
  task the agent wrote is shown as untrusted data, never as evidence);
  otherwise, or in a tainted session, it's saved paused with the reason, and the owner's
  `zen jobs resume` approves it. The agent can't set a job's model.
- **Running**: the loop sleeps until the earliest due job (at most `ZEN_JOBS_TICK`, 60 s), claims
  due jobs with `FOR UPDATE SKIP LOCKED` and sets their next run in the same transaction (an
  interval keeps its rhythm from the due time, `due + k·d`, so it doesn't drift); a partial
  unique index allows one `running` run per job, and a tick skips a job that has one; a running job counts as busy for upgrades. At
  start, runs left `running` are marked `interrupted`. A run later than its grace (half its period,
  2 minutes to 2 hours) is recorded `missed`; within it, it runs once (`catchup`). A failing agent
  job's next run waits at least 1, 5, 15, then 60 minutes, and the 5th failure in a row pauses it.
- **Schedules**: 5-field cron in the job's IANA zone (`croner`, `chrono-tz`; default
  America/Sao_Paulo, `ZEN_TZ`), `every <duration>`, `at <time>`, `in <duration>` (stored as `at`);
  agent jobs at most every 5 minutes (a cron's shortest gap over its next 50 runs). A duration
  or time out of range is an error, never a panic.
- `ZEN_JOBS=0` turns the scheduler off (smoke, e2e, eval and dev kernels); system jobs are still
  seeded.

### Memory, knowledge and skills

Phase 4 (D-069) separates curated experience, sourced knowledge and reusable procedures. The full
contract and owner commands are in [second brain](docs/second-brain.md). Short-term memory has a
bounded materialized view; pending captures are durable. Sleep promotes to long-term records or
archives expired detail, preserving revisions and sources without modifying USER/IDENTITY.
Knowledge/entity pages have stable identities, metadata and typed links; hash checks prevent stale
file revisions from being silently served. Skill revisions remain drafts until explicit owner review.

Per-turn evidence is selected from current memory, knowledge, related records, skill pointers and
searchable session archives. The fixed session prefix remains stable; historical bases replay as
recorded. Source ratings and writer dependencies are checked before handover. Receipts distinguish
bounded evidence selection from verified task completeness.

Search retains identifier, full-text and semantic arms, with English/Portuguese full-text indexes.
Existing flat wiki pages and capture remain compatible. The relationship registry supports people,
organizations and projects; meeting memories link to identities and preserve transcript references.
Skills remain folders loaded progressively. Delegation and scheduled jobs retain their existing
permission boundaries; memory does not create outreach or scheduling authority.

### System One

S1 (`crates/zend/src/s1/`, D-066) makes every fast decision: typed questions (yes/no, choice,
score) to an approved classifier on OpenRouter's `/api/v1/systemone` (Jev by default), answered
with probabilities; `bool` maps to `noul` on the wire. Each decision is a TOML file: its questions,
the evidence facts, a policy (what each mistake costs, ordered levels, or explicit rules), an
`unavailable` outcome, a ladder of approved models and test cases; the owner's
`~/.zenbot/s1/<name>.toml` replaces a built-in one once it passes the checker. The catalog in code
fixes each caller's facts, exposure and outcomes, so a decision on outside content can only lead
toward caution. Decisions today: `job_approval`, `owner_url`, `source_risk`, `content_risk`,
`task_check`, `web_focus`, `web_rerank`, `search_rerank`, `wiki_capture`, `skill_duplicate`,
`sleep_judge`, `sleep_recheck`, and `judge` (the brain's tool). Every call is recorded with its
model, latency and cost; the owner's answers to held actions and paused jobs label decisions, and
the nightly sleep fits calibration curves (applied where a file opts in) and proposes changes.
Private content is allowed unless `ZEN_S1_PRIVATE=0` (D-032). Configured by
`~/.zenbot/s1/models.toml` or `ZEN_S1_MODEL`, and `OPENROUTER_API_KEY`; without it each decision
takes its `unavailable` outcome, and a job the agent schedules is saved paused.

### Measurement

`turns` (one row per turn), `model_calls`, `tool_calls` (skill loads included), `envelopes`,
`session_decisions` (the owner's `/done` verdicts, with their source),
`decisions`, `memories`, `sleep_runs`. Evals: `scripts/eval.sh` runs two
harness versions with the same model on isolated kernels and databases (`evals/README.md`).

### Web and MCP

- **`web_fetch`** (`web.rs`): readable markdown (readability.js's algorithm via `dom_smoothie`,
  `htmd` for pages without an article), public addresses only (the kernel resolves names and keeps
  public addresses, IP literals and every redirect are checked, no proxy), 30 s / 5 MB / 20,000
  characters per call with `offset` paging and a 15-minute cache, `focus` keeping only the parts
  System One judges relevant, PDFs saved for `pdftotext`.
- **`web_search`**: `ZEN_SEARCH_PROVIDER` if set, else Brave (`BRAVE_API_KEY`) or Tavily
  (`TAVILY_API_KEY`), else SearXNG
  (`ZEN_SEARXNG_URL`, the compose service on 127.0.0.1:8888), which also rescues one failed keyed
  call; results deduplicated, http(s) only, reranked by System One.
- **Source judgments** (D-065, D-068; `sources.rs`): metadata-only lowering with a 30-day   assessed publisher-scope cache, separate raise-only content checks and owner-approved proposals.   Identifiable publishers with uncertain writers rate 3; unknown publishers remain 4. Owner   trust entries win over job-named source authority. Explicit owner approval belongs to a job's   unchanged definition and is revoked by agent edits. Public-suffix boundaries keep separate   tenants separate. A job's explicit workspace wins over inferred prompt/report folders.
- **Alignment and queue** (`gate.rs`, `task.rs`, `queue.rs`): effects are checked against the   authenticated root-owner goal or unchanged stored job prompt, with only pre-exposure assistant   plans admitted. Local work skips the judge. At high risk, aligned file edits run while shells   remain read-only; contradicting actions lock, unavailable or ambiguous judgments hold. A hold   never contaminates. Approve grants exact effects once with a kernel-origin retry; reject is   terminal; deny contaminates the session and dependent readers. A held scheduled run waits up   to two hours, then waits for the decision's continuation to finish before delivering its report.
- **Outside content** (`taint.rs`, `sources.rs`): numeric origin ratings survive copying and   persistence; reads at 2+ are labelled. Denial and level-5 content stop readers. `/trust` lifts   reviewed causes forward only; stored products retain their origin. `fsview.rs` masks products   of contaminated writers. Prompt files, active skills, made tools, owner config and secrets   retain their separate write protections. Every shell runs contained except an authorized host   command (D-067); high-risk workspaces are read-only with private scratch space. Permission   snapshots are shared within each tool call, invalidated on mutations, and never retained over   another call. Handover checks contamination live; database failures fail closed.
- **MCP** (`mcp.rs`, D-033, D-052): servers in `~/.zenbot/mcp.json` (stdio or streamable HTTP,
  `${VAR}` from the environment, a secret-named one only when the server lists it in `allow_env`,
  `include`/`exclude`, `timeout_s`, `untrusted`, default true for every server until the owner
  sets `"untrusted": false`). The tool list never changes: `find_tools` ranks tools by name and description,
  `load_tool` returns the schema, `call_tool` checks required arguments, runs the call with a
  timeout, masks and caps the output (the rest to `~/.zenbot/outputs`), and wraps untrusted output.
  Calls run concurrently, to one server and to several: a server's lock is held only to connect,
  tools are looked up from its last listing (listed again on `tools/list_changed`), and a stdio
  server's answers reach their calls by id. A message over 8 MB fails its call. A stdio server runs
  in its own process group, killed whole when it is replaced or the kernel stops.
  Stdio servers run contained with a private writable cache folder and network through a proxy
  evaluated at the server's own trust level. `GET /api/mcp` reports cached counts and errors without
  starting servers. `zen trust suggest` proposes repeated clean hosts for explicit owner review.

### Security today

One owner token for the API. The listener binds to loopback by default (`ZEN_BIND` opts into an
external address); the token goes only in the `Authorization` header, never the URL. Every tool runs in the kernel; a
verifier's shell runs read-only in bubblewrap with its own processes (the kernel's `/proc` out of
sight), no network and an empty home with only the work bound back and the secret files hidden;
its `read` stays inside the work. Worker shells do not inherit kernel secret variables. Workers
get none of them either (every secret-named variable is removed, except the engines' sign-ins),
and start without a login shell. Made tools
run from a private copy of their folder, without network and with an empty home until the owner
approves the content they reviewed. What untrusted content is, how it's wrapped, what a tainted
session may not do (its shell offline, D-053) and which files `edit`/`write` protect (D-051) are in
"Untrusted content" above.
Engines are checked at run time, not only configured: a Claude Code run (a turn or a tool-free
completion) must start with `system/init` listing its tools, none but zenbot's `mcp__zen__*`, or it
is refused. Codex turns, model listings and tool-free completions use a private `CODEX_HOME` (only
the owner's sign-in is linked) and disable Codex's tool features, MCP servers, web search and
project docs; a turn in which Codex starts anything but a message, reasoning, a plan, a compaction
or a zenbot dynamic tool call (its own shell, file changes, MCP, web search, …) is stopped and fails
naming it. The daily `engines` job also fails when a Codex release turns on a feature zenbot hasn't
reviewed (`zen-engine codex-features`).
All tool output is masked before reaching the model or tape, and tool arguments before clients or
`tool_calls` see them; full outputs stay under `~/.zenbot/outputs`. The database user has a
private password (`~/.zenbot/db-password`, set by `db_secure` in `scripts/db.sh` at install and on
upgrade; the bootstrap `zen` in `deploy/compose.yaml` is only for the first start, CI and throwaway
setups), so other processes on the VM can't use Postgres without reading the owner's files.
**Every session's shell is contained (D-067):** bubblewrap with the machine read-only, the
workspace writable unless the session's context is high or it's locked, the zen home read-only with
its secrets hidden, private `/tmp` and `/run` (no Docker or other host sockets), its own processes
and `no_new_privs` (sudo and setuid fail), and its network only through the kernel's per-session
egress proxy (`zend bridge` inside; each new host decided by S1's `egress`, recorded). Acting as
the owner (push, gh, ssh, docker, systemctl) needs `host: true`, which the action gate checks: S1's
`action_gate` over the context risk (the trust list, S1 source ratings, the content scan), the
action's effect and, when needed, alignment with the owner's job, giving run, hold or lock. A host
command that runs, and `zend` itself, still run as the owner's Unix user, who has passwordless
`sudo` and Docker on the standard install; a separate uid is later hardening (ROADMAP.md). MCP stdio
servers use the same containment boundary with their own writable cache folder and trust policy.

### Deployment

- `deploy/compose.yaml` runs Postgres (pgvector) and SearXNG (keyless web search). `zend` runs on the host as the systemd service
  `zenbot` (installed by `install.sh`) and starts its workers. One port for the API and the WebSocket;
  `/health` reports the database, workers and busy sessions. `zen-matrix` is a separate
  workspace, built and installed by `scripts/matrix.sh` (not by CI's release), with its own systemd
  service.
- The kernel's `engines` job updates the Claude Code and Codex CLIs daily, tested, with rollback;
  its `sleep` job runs the memory sleep nightly ("Scheduled jobs"). The systemd timers that ran
  them before D-046 are removed by `install.sh` and by the first healthy upgrade.
- CI publishes binaries for every commit on `main` that passes its checks; installs and upgrades
  download them or compile. Changes are installed only by `scripts/upgrade.sh` (DEVELOPMENT.md,
  "Applying a change").
- `INSTALL.md` is written for a coding agent to follow on a fresh Linux VM.
- Not built yet: `zend` in compose, a reverse proxy with TLS, a sandbox image, nightly off-VM backups.

---

## Target design (agreed 2026-10-06)

Not built yet unless marked; most of it has shipped since (see "As built"). The roadmap builds it
in phases. Sources: the Phase 0 research in
`docs/archive/research/` (Hermes, OpenClaw, agentskills.io, Anthropic's tool search, FastMCP, rmcp, Voyager,
Letta, LLM Wiki and gbrain, Postgres hybrid search, web providers and fetch safety), each claim with
file paths in the projects' code.

### Principle

Give the agent tools, skills and context; don't push it through a workflow (D-026). Fixed rules only
for authority (the owner starts jobs and sets the budget; the agent never decides the owner's calls),
safety (every action through the kernel, sandboxed, secrets masked, audited) and measurement (the
tape). A rule comes back only where measurement shows the agent needs it.

### Engine

- **Provider-free.** Claude Code and Codex on subscriptions, System One classifiers such as
  Jev through OpenRouter; always the latest. zenbot owns skills, tools and context, so any model can do any session or
  subtask. Engine-native skills and tools are not used.
- **Context management stays as built.** A model switch re-sends the whole context, so switches
  happen at natural boundaries (a new session or subtask).
- **Only what the job needs enters the context.** Everything loaded after session start is appended
  as a tool result; the prefix never changes mid-session. Every load is recorded per turn, and what is
  loaded but never used is measured and steered away. A skill or tool loaded in most sessions of a
  kind can move into that kind's prefix, on data.

### What the agent gets, and when

| When | What |
|---|---|
| Session start (fixed prefix) | `SOUL.md` · `IDENTITY.md` · `AGENTS.md` · `USER.md` · `MEMORY.md` (fixed size) · the system tools with their descriptions · a short index of skill domains · a repo's own `AGENTS.md` as project context |
| On demand (appended) | skills, connected MCP servers' tools, tools the agent made |

| File in `~/.zenbot/` | Says | Owner |
|---|---|---|
| `SOUL.md` | the deep layer: what the agent is for, what it holds to, lines it doesn't cross | owner only; agent may suggest |
| `IDENTITY.md` | the agent in practice: character, how it works, learned guidance (D-045) | agent writes what really matters, kept compact; the owner may edit |
| `AGENTS.md` | its environment: the VM, its body, where things live, what it can reach | owner; agent proposes |
| `USER.md` | the owner: who they are, their preferences, their context | owner; agent writes what really matters, kept compact |
| `MEMORY.md` | where things stand (not traits or preferences) | agent, within a fixed size |

**Who teaches what:** `AGENTS.md` the environment; each tool's own description how and when to use
that tool (what it does, when to use it and when not, what it returns, an example); skills how to do
a kind of work well. Tool descriptions are paid on every turn, so they grow only where the model is
measured misusing a tool.

### System tools

Loaded at session start.

| Tool | What it does | Today |
|---|---|---|
| `bash` | Run commands in the sandbox | exists |
| `read` | Read a file or image | exists |
| `write` | Create or overwrite a file | exists |
| `edit` | Exact string replacement in a file | exists |
| `ask` | Bring the owner 1–3 questions, each with 2–4 options, recommended first; unanswered → the recommendation, recorded as an assumption. `wait: false` keeps working on what doesn't depend on the answer | built (D-038 added `wait: false`) |
| `search` | One search across sessions (this one included), memories and the wiki | built (D-035, D-036) |
| `remember` | Add, replace or remove a short-term memory entry, with its source | built |
| `web_search` | Search the web through the configured provider | built |
| `web_fetch` | Fetch a URL as readable text, with its links | built |
| `find_skills` | Search skills by need: names and one-line descriptions | built (word match; System One ranking later) |
| `load_skill` | Load a skill, or one of its reference files | built |
| `save_skill` | Create or improve a skill, from evidence | built (D-037) |
| `find_tools` | Search MCP and agent-made tools by need: names and one-line descriptions | built (word match) |
| `load_tool` | Load a tool's full definition so it can be called | built |
| `call_tool` | Run a loaded tool (the list stays fixed, D-033) | built |
| `save_tool` | Make a tool: a script plus a manifest, sandboxed until the owner approves | built (D-037) |
| `judge` | Ask System One one question over many items, with a probability each | built (D-066) |
| `verify` | A fresh verifier checks work against criteria, without the maker's reasoning | built |
| `capture` | Put a concept into the wiki | built (D-036) |
| `delegate` | Hand subtasks to subagents with fresh context and a chosen model, in parallel | built (D-038) |
| `schedule` | Create, change or remove a scheduled job | built (D-046) |

Gone (2026-10-07): `move` (`bash mv`), `propose_brief` (a brief is a file the `brief` skill
writes), `submit_work`, `note_ruling` and the approvals. Going: `history`, once `search` covers it
(`search` shipped in D-035; `history` is still offered, and reads messages by number). Skills and
made tools are authored through `save_skill` and `save_tool`, which check the format and the
rules (D-037); wiki pages through `capture` (timeline) and `edit` (the summary).

### System One, used heavily

- **Called by the brain** through `judge`: one question over up to 200 items, an answer and a
  probability each, instead of reading and judging many things itself (D-066).
- **Built into tools** as decision files, so every engine benefits: `search` and `web_search` rank
  by relevance, `web_fetch` keeps the parts that help, the sleep judges memories, `capture` routes,
  sources and content are rated, risky actions are checked against the job. `find_skills` and
  `find_tools` still match words.
- **Recorded and learned from:** every decision with its probabilities and, when known, what really
  happened; calibration applies where a file opts in, and changes reach a file only through the
  owner's approval.
- System One decides (where, whether, which); generative work (writing text) goes to a model.
- **Private content:** System One may see it unless the owner sets `ZEN_S1_PRIVATE=0` (D-032).

### Memory and knowledge

Implemented structure and lifecycle: D-069 and [second brain](docs/second-brain.md). The knowledge
graph is a revisable understanding with evidence; experiences are memory; session archives are
sources. Retrieval selects bounded context and reports gaps. A single search never proves complete
coverage, and consolidation never grants authority.

### Skills and tools that improve themselves

**Skills** use the agentskills.io format (`SKILL.md` with frontmatter, `references/`, `scripts/`) in
`~/.zenbot/global/skills/<domain>/<skill>/`, in git. Rules against sprawl (D-029): domains first (a new
domain needs the owner's OK); edit before create (search first; a new skill only when none covers
the work, with the reason recorded); changes from evidence at session close, never from one task;
small and composable; loads and outcomes measured, near-duplicates merged, unused skills retired;
every change a revertible commit. `save_skill` and the nightly `skills` job enforce these (D-037).

**MCP client** written against the spec (D-033; built). From FastMCP: namespacing and mounting
(`<server>_<tool>`, built), and not built yet: tool transformation (rename, hide arguments, rewrite descriptions), middleware
(audit, permissions, secret injection), proxying (zenbot's own tools as an MCP server, SPEC.md §3).

**Agent-made tools** are a script plus a manifest (name, input schema, command), run by the kernel in
the sandbox and kept in git; a full MCP server only when a tool must keep state. A new tool gets no
network until the owner approves, and never the kernel's secrets (built, D-037).

**Loading tools mid-session** would change the tool list, which rewrites the cached prefix on every
engine (the Phase 0 spike), so the list stays fixed and `call_tool` runs whatever `load_tool`
showed (built).

### Model choice

Learned from real usage, not fixed tasks (D-030, Phase 6): a versioned policy maps the kind of work
to a model (subtasks take the `unknown` route since System One stopped classifying them, D-066); a small share of subtasks explore another model with the choice
probability logged; outcomes (verdicts, corrections, verification, cost) update the policy. Built
for subagents (D-038, "Delegation" above: the owner's verdicts and cost are the outcomes so far); a
main session runs on the model the owner picks.
