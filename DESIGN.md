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
    zen-engine (Rust)                    SQLite + sqlite-vec / OpenRouter
    Claude Code CLI ─ MCP bridge ─┐      (private local database file)
    Codex app-server ─ dynamic    │
    tools                         └── tools come back to zend
```

| Process | Language | Responsibility |
|---|---|---|
| `zend` | Rust (axum, sqlx, tokio) | Always-on kernel; owns state and side effects; single binary |
| `zen` | Rust | Terminal app (full screen with a sessions board as home (`GET /api/board`, D-048), diffed frames, scrolling and a file side panel, or `--inline`) and script commands (`--json`) |
| `zen-matrix` | Rust (matrix-sdk) | Optional Matrix channel (D-049, `docs/matrix.md`): a client of the client protocol like `zen`, one encrypted room per session plus a "zen jobs" room for job reports; systemd service `zen-matrix`, set up by `scripts/matrix.sh` |
| `zen-engine` | Rust | Default worker: runs turns on the Claude Code CLI and `codex app-server` on the owner's subscriptions, with zenbot's prompt, tools and history |
| SQLite | C, bundled | Private WAL database; sqlite-vec and FTS5, `DATABASE_URL` override |

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
  upkeep ten minutes apart: `wiki` (03:10, `brain::tend`: index and lint records, report issue counts), `skills` (03:20, `workshop::tend`: flag skills unused
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

Three curated stores (D-069): memory (what matters from lived work), knowledge (understanding of a
subject and how we know it) and skills (how to do a task well). People, organizations and projects
are records shared by all three. Session history is a searchable archive, not memory: nothing
reaches a turn on its own; the brain looks things up with `search`. Contract and formats:
[docs/second-brain.md](docs/second-brain.md).

- **Short-term memory** (`memory.rs`, table `memories`): one capped file, `global/MEMORY.md`,
  rendered into the instructions at a session's start (frozen for the session; writes show from the
  next one). `remember` adds, replaces or removes an entry with its kind and source (`owner` only
  for the owner's exact words). After each turn with the owner, System One picks sentences of the
  owner's message worth keeping (`memory_capture`). The owner may edit the file; the kernel reads
  the edits back. Writes are never refused: past `ZEN_MEMORY_CHARS` (4000) a sleep starts at once.
  An entry at risk 4 or above, or with a contaminated writer (any session that wrote or replaced
  it), is withheld from instructions by text (only its id is listed); ratings 2–3 enter labelled.
- **Long-term memory, knowledge and entities** (`brain/`): one Markdown file per record with a
  JSON frontmatter: tags, aliases, subjects, evidence with its basis (proof, empirical,
  interpretation, tradition…) and typed links (supports, contradicts, supersedes, about…). Long-term
  memory in `global/memory/`, people/organizations/projects in `global/entities/`, knowledge in
  folders by kind under the wiki. The files are the source of truth; the database indexes them and
  is rebuilt from them. The agent writes through `brain` put (revision-checked, the old version
  backed up) and never edits these files directly; a file's rating is its bytes' (D-067).
- **Sleep** (`brain/sleep.rs`, the kernel's `sleep` job nightly at 03:00 UTC, at once when memory
  passes its size, `zen memory sleep`): System One judges each short-term memory: lasting ones move
  to long-term memory as records citing them (`memory_durable`), finished ones are archived
  (`memory_disposition`), the rest stay. `remember` with `long: true` writes a long-term memory on the
  spot only past a high bar (`memory_long`). A memory changed while the sleep runs is left alone; one
  whose writer was denied is archived without a model. Nothing is deleted and no prompt file is
  touched. The run is a `sleep_runs` row; the next sessions get a one-line note.
- **Search** (`search.rs`, D-035): an indexer keeps `search_docs` current: one document per turn of
  every session (the owner's words, the answers, the tools called; no tool output), one per
  short-term memory (and each `long` row from before D-045), wiki page and record file. Memories come through a
  queue their changes fill in the same transaction (no time watermark, so a long transaction such as
  the sleep is never skipped). Embeddings are made outside the indexing lock, so a search never waits
  on the provider; a turn is embedded once it has ended (or has been still for 2 minutes), and a
  document the provider refuses is isolated by halving its batch and left to text search after 3
  tries. `search` runs exact names and paths first (literal identifier matching),
  then full text (FTS5 Unicode words with accent folding) and meaning (sqlite-vec; `ZEN_EMBED_MODEL`, default
  `openai/text-embedding-3-small` through OpenRouter, filled in the background; off with
  `ZEN_EMBED=0` or `ZEN_S1_PRIVATE=0`) merged by reciprocal
  rank fusion, each arm filtering the table through its own index (trigram, GIN, HNSW), and System
  One reranks (`search_rerank`). From the records it keeps, search follows typed links: linked
  records join when System One keeps them too, two hops at most; a `contradicts` link is always
  followed one hop and a superseded record gives way to the newer one. Each result says how it was
  reached and lists its links. A memory found
  counts as used. Every search is a `searches` row. `history` reads any session's messages by
  number. Results from a tainted session's turns, a marked memory or a wiki page with an entry
  labelled `web` come back as untrusted content and taint the searching session; so does `history`
  of a tainted session.
- **Legacy wiki** (`wiki.rs`, D-070): old flat pages remain searchable until the owner migrates
  them (`brain/legacy.rs`): preview, then apply. Each becomes a provisional source record retaining
  the full original, metadata and resolvable links, with exact originals privately archived and all
  writers and ratings preserved. Conflicts stay held; retries never overwrite records. `capture`
  is retired. The compatibility nightly `wiki` job indexes and lints records, reporting counts.
- **Workshop** (`workshop.rs`, D-037): `save_skill` (reason required, format and size checked,
  near-duplicates refused; new skills and revisions of active ones are drafts in `skills/_proposed`
  until the owner accepts them by fingerprint (D-069), new domains the owner's call, commits; the
  nightly `skills` job flags and archives unused skills). Made-tool authoring and execution are
  retired; existing files remain inspectable, and old approvals cannot activate them (D-070).
  The owner accepts or rejects a skill draft by the
  fingerprint `zen skills` showed, refused when the content changed since (D-051). A draft a
  tainted session wrote (`save_skill`, or `edit`/`write` into it) is marked in `draft_taint`
  before it's written and in its commit; `find_skills`, `load_skill` and `read` give it as
  untrusted content that taints the reader, `zen skills` shows it `untrusted`, and only the owner's
  own accept activates it (D-053). Each `load_skill` records the version loaded
  (`brain_skill_uses`) and the owner's verdict on the session.
- **Delegation** (`delegate.rs`, D-038): subagents (kind `subagent`, no `ask`, `delegate` or
  `schedule`; they inherit the parent's taint), several
  tasks per call in parallel; a child (subagent or verifier) is stopped with the turn that waits
  for it (an abort, the turn ending), and so on down; the model from the routing policy (`policies`) by kind of work, with
  logged exploration; the nightly `routes` job tunes routes on clear evidence.
- **Skills** (`skills.rs`): folders in `~/.zenbot/global/skills/<domain>/<name>/` in the agentskills.io
  format, validated when scanned (invalid ones are skipped and logged). The instructions carry an
  index; `find_skills` matches names, descriptions and bodies; `load_skill` returns a SKILL.md or a
  file inside the skill (never outside it) as a tool result.

### System One

S1 (`crates/zend/src/s1/`, D-066) makes every fast decision: typed questions (yes/no, choice,
score) to an approved classifier on OpenRouter's `/api/v1/systemone` (Jev by default), answered
with probabilities; `bool` maps to `noul` on the wire. Each decision is a TOML file: its questions,
the evidence facts, a policy (what each mistake costs, ordered levels, or explicit rules), an
`unavailable` outcome, a ladder of approved models and test cases; the owner's
`~/.zenbot/s1/<name>.toml` replaces a built-in one once it passes the checker. The catalog in code
fixes each caller's facts, exposure and outcomes, so a decision on outside content can only lead
toward caution. Decisions today: `job_approval`, `owner_url`, `source_risk`, `content_risk`,
`task_check`, `web_focus`, `web_rerank`, `search_rerank`, `skill_duplicate`,
`memory_capture`, `memory_durable`, `memory_disposition`, `memory_long`, and `judge` (the brain's
tool). Every call is recorded with its
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
`tool_calls` see them; full outputs stay under `~/.zenbot/outputs`. The SQLite database, WAL/SHM and recovery artifacts are private kernel-only files; contained
shells cannot see them and structured file tools refuse direct access. PostgreSQL credentials
remain secret while retained for migration/recovery. Same-user host access remains the broader
boundary described below.
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

- SQLite is embedded; no database daemon or Docker is required. Search uses configured external
  SearXNG or Brave/Tavily. `zend` runs on the host as the systemd service
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

## Historical design

The [2026-10-06 target](docs/archive/DESIGN-target-2026-10-06.md) is preserved as history.
Current behavior is documented above; remaining work is in ROADMAP.md.
