# zenbot — Design

> How the system is built and how it works: first as it is today, then the target design agreed on
> 2026-10-06 (D-025 to D-030) that the roadmap builds. What zenbot is meant to have, module by module,
> is in [SPEC.md](SPEC.md); where each piece lives in the code, in [MAP.md](MAP.md); the order of
> work, in [ROADMAP.md](ROADMAP.md). Deep dives: `docs/context.md` (what the model reads each turn),
> `docs/brief.md` (briefs and verification), `docs/worker-protocol.md`,
> `docs/client-protocol.md`.

## Invariants

These hold today and in the target design. Changing one needs the owner's OK and a DECISIONS.md entry.

1. **The kernel owns all state and runs every tool call.** Workers are stateless: they get a turn's
   context and ask the kernel to run tools. Engines run with their own tools off (Claude Code
   `--tools ""`, Codex shell disabled), so every action goes through the kernel.
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
 zen (terminal app, script commands)        web UI (frozen)
          │ HTTP + WebSocket, one port (ZEN_PORT, default 8100), owner token
┌──────────────────────── zend (Rust kernel, systemd service `zenbot`) ────────────────────────┐
│ API/WS · sessions and tape · context compiler · tools and executor · memory and sleep · skills │
│ System One decisions and scoring · summaries · tracing · worker routing and supervision        │
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
| `zen` | Rust | Terminal app (full screen with diffed frames, scrolling and a file side panel, or `--inline`) and script commands (`--json`) |
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
`envelope`, `compaction`, `engine_session`, `base` (the session's instructions), `questions` and
`verification` (and, from sessions before 2026-10-07, the old workflow's `state`, `brief`,
`submission`, … blocks).

What the model reads each turn (`docs/context.md`): the envelope (system prompt and tools, fixed for
the session) → a summary of older turns, if any → the history (append-only) → the new message with
its turn context (the date) at the end, sent only when it
changed. The system prompt is the prompt files (laid out by scope, `layout.rs`, D-040) `~/.zenbot/agents/zenbot/SOUL.md`, `AGENTS.md` (the environment)
and `USER.md`, short-term memory as of the session's start, the skills index, then `AGENTS.md` (or
`CLAUDE.md`) from `/` down to the workspace; a project's file found later by a tool is attached to
that tool result and kept. The kernel writes missing default prompt files and skills at start
(`crates/zend/defaults/`), never overwriting. Each zenbot session keeps a matching Claude Code session (`--resume`) or Codex thread, so
earlier turns come from the provider's cache. Past 70% of the context budget, older turns are
summarized in the background with block addresses; the `history` tool reads any block back.

### Tools today

Fixed order, the same every turn of a session: `bash`, `read`, `write`, `edit`, `history`, `ask`,
`remember`, `web_search`, `web_fetch`, `find_skills`, `load_skill`, `find_tools`, `load_tool`,
`call_tool`, `verify`, and `decide` when a System One model is configured. A verifier session gets only `bash` (read-only, bubblewrap), `read` and
`submit_verdict`. Each description says what the tool does, when to use it and when not, and what
it returns. Tool output is cut once, when the tool runs (full output saved and referenced); edits
are serialized per file and CRLF/BOM-safe; tools are cancelled with their process group on abort.

### Briefs and verification

A skill and a tool, not a kernel workflow (D-026, `docs/brief.md`): the `work/brief` skill frames a
big, risky or unclear job; the `verify` tool has the kernel run the criteria's commands and, for
criteria that need judgment, a fresh read-only verifier session judge the diff. `ask` ends the turn
with questions for the owner. The kernel-enforced workflow (`flow.rs`) was removed on 2026-10-07.

### Memory and skills

- **Short-term memory** (`memory.rs`, table `memories`): `remember` adds, replaces or removes an
  entry with its source (`owner`, `verified`, `inferred`). Rendered into the instructions at a
  session's start (frozen for the session) and exported to `~/.zenbot/global/MEMORY.md`. Size
  `ZEN_MEMORY_CHARS` (4000); writes past twice that are refused and start a sleep at once.
- **Sleep** (`memory::sleep`, `scripts/sleep.sh` from `zen-sleep.timer` nightly, `zen memory
  sleep`): ranks entries (System One's "needed soon" when allowed, else recency; the owner's words
  and verified results a little higher), keeps what fits, archives the rest, and proposes for
  long-term the entries from the owner or a check that System One judges durable and impactful at
  ≥ 0.95 on the lowest of three samples. Proposals wait for the owner (`zen memory accept|reject`);
  promotion acts on its own once the Wilson lower bound of the owner's agreement with its proposals
  reaches 0.95 (D-035; `ZEN_MEMORY_PROMOTE=on|shadow` overrides). Every entry's fate
  is a `decisions` row; the run is a `sleep_runs` row; the next sessions get a one-line note. System
  One sees memories unless `ZEN_S1_PRIVATE=0` (D-032).
- **Search** (`search.rs`, D-035): an indexer keeps `search_docs` current: one document per turn of
  every session (the owner's words, the answers, the tools called; no tool output) and one per
  short- or long-term memory. `search` runs exact names and paths first (trigram over identifiers),
  then full text (`simple`) and meaning (pgvector; `ZEN_EMBED_MODEL`, default
  `openai/text-embedding-3-small` through OpenRouter, filled in the background) merged by reciprocal
  rank fusion, and System One reranks. Long-term memories are reached this way; a memory found
  counts as used. Every search is a `searches` row. `history` reads any session's messages by
  number.
- **Wiki** (`wiki.rs`, D-036): pages in `~/.zenbot/global/wiki/` (git) with a summary over an append-only
  dated timeline; `capture` finds candidate pages with search, System One picks the page (or a new
  one) and catches duplicates and sensitive notes, the kernel appends, masks, labels web-sourced
  notes, keeps `index.md` and `log.md`, and commits; the agent writes the summary. Indexed for
  `search`; linted and committed by the nightly sleep.
- **Workshop** (`workshop.rs`, D-037): `save_skill` (reason required, format and size checked,
  near-duplicates refused, drafts in `skills/_proposed` until the owner or an accepted session
  vouches, new domains the owner's call, commits; the sleep flags and archives unused skills) and
  `save_tool` (made tools in `~/.zenbot/global/tools/`, called as `made_<name>`, sandboxed without network
  until approved in `made_tools`).
- **Delegation** (`delegate.rs`, D-038): subagents (kind `subagent`, no `ask` or `delegate`), several
  tasks per call in parallel; the model from the routing policy (`policies`) by kind of work, with
  logged exploration; the sleep tunes routes on clear evidence.
- **Skills** (`skills.rs`): folders in `~/.zenbot/global/skills/<domain>/<name>/` in the agentskills.io
  format, validated when scanned (invalid ones are skipped and logged). The instructions carry an
  index; `find_skills` matches names, descriptions and bodies; `load_skill` returns a SKILL.md or a
  file inside the skill (never outside it) as a tool result.

### System One

A fast typed-decision model (Jev via OpenRouter's `/api/v1/systemone`, called by the kernel):
choice, score or bool questions, answered with probabilities. Public bool maps to `noul` on the
wire; OpenRouter reports usage and cost directly. Used today for live scoring of sessions (from the owner's
messages and final answers only, never tool output; `session_scores`), the model's `decide` tool,
and the memory sleep (private content allowed unless `ZEN_S1_PRIVATE=0`, D-032). Configured
by `ZEN_S1_MODEL` and `OPENROUTER_API_KEY`; no additional worker is needed.

### Measurement

`turns` (one row per turn), `model_calls`, `tool_calls` (skill loads included), `envelopes`,
`session_decisions` (the owner's `/done` verdicts, with their source), `session_scores`,
`decisions`, `memories`, `sleep_runs`. Evals: `scripts/eval.sh` runs two
harness versions with the same model on isolated kernels and databases (`evals/README.md`).

### Web and MCP

- **`web_fetch`** (`web.rs`): readable markdown (readability.js's algorithm via `dom_smoothie`,
  `htmd` for pages without an article), public addresses only (the kernel resolves names and keeps
  public addresses, IP literals and every redirect are checked, no proxy), 30 s / 5 MB / 20,000
  characters per call with `offset` paging and a 15-minute cache, `focus` keeping only the parts
  System One judges relevant, PDFs saved for `pdftotext`.
- **`web_search`**: Brave (`BRAVE_API_KEY`) or Tavily (`TAVILY_API_KEY`), else SearXNG
  (`ZEN_SEARXNG_URL`, the compose service on 127.0.0.1:8888), which also rescues one failed keyed
  call; results deduplicated, http(s) only, reranked by System One.
- **Untrusted content** (D-034): results are wrapped in `<untrusted …>` (markers inside are
  defused) and the session is tainted (`sessions.tainted_at`, a `taint` block); `remember` from a
  tainted session records `inferred`, so web text can't become a promotable memory.
- **MCP** (`mcp.rs`, D-033): servers in `~/.zenbot/mcp.json` (stdio or streamable HTTP, `${VAR}`
  from the environment, `include`/`exclude`, `timeout_s`, `untrusted`, default true for remote
  servers). The tool list never changes: `find_tools` ranks tools by name and description,
  `load_tool` returns the schema, `call_tool` checks required arguments, runs the call with a
  timeout, masks and caps the output (the rest to `~/.zenbot/outputs`), and wraps untrusted output.
  `GET /api/mcp` shows servers and problems.

### Security today

One owner token for the API. The listener binds to loopback by default (`ZEN_BIND` opts into an
external address); only a WebSocket upgrade accepts `?token=`. Every tool runs in the kernel; a
verifier runs read-only in bubblewrap. Worker shells do not inherit kernel secret variables, and
the verifier hides token files. Made tools run without network and with an empty home until approved.
Codex turns and tool-free completions use a private `CODEX_HOME` (only the owner's sign-in is linked)
and disable Codex's built-in tools, MCP servers and project docs.
Web pages and untrusted MCP output (including tool descriptions) taint the session and are wrapped.
All tool output is masked before reaching the model or tape; full outputs stay under
`~/.zenbot/outputs`. **This is not a security boundary against the agent:** ordinary commands still
run as the owner's Unix user and can read the owner's files, including `~/.zenbot/token`. Strong
isolation needs a separate uid and an owner credential unavailable to it. Outward-facing actions
are covered by the system prompt ("ask before"), not enforced.

### Deployment

- `deploy/compose.yaml` runs Postgres (pgvector) and SearXNG (keyless web search). `zend` runs on the host as the systemd service
  `zenbot` (installed by `install.sh`) and starts its workers. One port for API, WebSocket and web
  UI; `/health` reports the database, workers and busy sessions.
- `zen-engines.timer` updates the Claude Code and Codex CLIs daily, tested, with rollback.
  `zen-sleep.timer` runs the memory sleep nightly. Both are installed by
  `install.sh` and refreshed after each upgrade.
- CI publishes binaries for every commit on `main` that passes its checks; installs and upgrades
  download them or compile. `scripts/upgrade.sh` checks, smoke-tests on a throwaway database copy,
  backs up before migrations, restarts when no session is busy, and rolls back if unhealthy
  (DEVELOPMENT.md).
- `INSTALL.md` is written for a coding agent to follow on a fresh Linux VM.
- Not built yet: `zend` in compose, a reverse proxy with TLS, a sandbox image, nightly off-VM backups.

---

## Target design (agreed 2026-10-06)

Not built yet unless marked. The roadmap builds it in phases. Sources: the Phase 0 research in
`docs/research/` (Hermes, OpenClaw, agentskills.io, Anthropic's tool search, FastMCP, rmcp, Voyager,
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
| Session start (fixed prefix) | `SOUL.md` · `AGENTS.md` · `USER.md` · `MEMORY.md` (fixed size) · the system tools with their descriptions · a short index of skill domains · a repo's own `AGENTS.md` as project context |
| On demand (appended) | skills, connected MCP servers' tools, tools the agent made |

| File in `~/.zenbot/` | Says | Owner |
|---|---|---|
| `SOUL.md` | who the agent is: character, standards, how it works with the owner | owner; agent proposes |
| `AGENTS.md` | its environment: the VM, its body, where things live, what it can reach | owner; agent proposes |
| `USER.md` | the owner: who they are, their preferences, their context | owner; agent proposes |
| `MEMORY.md` | short-term memory | agent, within a fixed size |

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
| `ask` | Bring the owner 1–3 questions, each with 2–4 options, recommended first; unanswered → the recommendation, recorded as an assumption. `wait: false` keeps working on what doesn't depend on the answer | built (ends the turn; `wait: false` in Phase 6) |
| `search` | One search across sessions (this one included), memories and the wiki | built (sessions, memories; the wiki in Phase 4) |
| `remember` | Add, replace or remove a short-term memory entry, with its source | built |
| `web_search` | Search the web through the configured provider | built |
| `web_fetch` | Fetch a URL as readable text, with its links | built |
| `find_skills` | Search skills by need: names and one-line descriptions | built (word match; System One ranking later) |
| `load_skill` | Load a skill, or one of its reference files | built |
| `find_tools` | Search MCP and agent-made tools by need: names and one-line descriptions | built (MCP) |
| `load_tool` | Load a tool's full definition so it can be called | built |
| `call_tool` | Run a loaded tool (the list stays fixed, D-033) | built |
| `decide` | Ask System One typed questions, in batches, with probabilities | built |
| `verify` | A fresh verifier checks work against criteria, without the maker's reasoning | built |
| `capture` | Put a concept into the wiki | built (D-036) |
| `delegate` | Hand subtasks to subagents with fresh context and a chosen model, in parallel | built (D-038) |

Gone (2026-10-07): `move` (`bash mv`), `propose_brief` (a brief is a file the `brief` skill
writes), `submit_work`, `note_ruling` and the approvals. Going: `history` (once `search` exists). Wiki pages, skills and
tool manifests are files, so `write` and `edit` cover authoring; the kernel validates the format on
save.

### System One, used heavily

- **Called by the model** through `decide`: whenever the answer is one of known options, whenever
  there are many items, whenever a cheap second opinion helps. Batches of items with a probability
  each, so the model sets its own threshold.
- **Built into tools**, so every engine benefits: `search`, `web_search` and `find_*` rank and filter
  by relevance; `web_fetch` can keep only the relevant parts; `remember`'s sleep scores entries;
  `capture` routes and de-duplicates.
- **Shadow first.** Each new use is logged in `decisions` with its probabilities and, later, what
  actually happened; it acts on its own once it matches outcomes often enough.
- System One decides (where, whether, which); generative work (writing text) goes to a model.
- **Open:** whether System One may see private content (ROADMAP.md, open decisions).

### Memory and knowledge

| Kind | In zenbot | Where |
|---|---|---|
| Working | `MEMORY.md`, fixed size | `memories` table, rendered into the prompt and exported as a file |
| Episodic (what happened) | sessions, the tape | Postgres (built) |
| Semantic (what's true) | long-term memories; the wiki | memories in Postgres (source, supersedes); wiki as markdown in git |
| Procedural (how to do things) | skills, tools | markdown and scripts in git |
| External | `web_search`, `web_fetch` | web content is untrusted input (taint rule, SPEC.md §5.18) |

**Short-term memory.** Anything can be saved with `remember`; each entry keeps its text, source (the
owner's words, a verified result, or the agent's inference), when it was made and when it was last
used. `MEMORY.md` is rendered at session start and frozen for the session; writes show from the next
one. Entries may exceed the size during the day up to a hard ceiling (about 2×), which triggers an
immediate tidy-up.

**Sleep hygiene (nightly).** The space is fixed, so entries compete for it. A nightly job (fixed token
budget, cost recorded) asks System One about every entry:

| Question | Type |
|---|---|
| Will this be needed in the coming days? | score |
| Will it still be true in months? | probability |
| Does it change how the agent should act for the owner, across jobs? | score |
| Is it already covered (another memory, a skill, `USER.md`)? | probability |

- **Keep:** rank by likely need, adjusted for recent use; fill the fixed size from the top.
- **Promote to long-term:** only really impactful memories. Durable and impactful at a very high bar
  (e.g. ≥ 0.95) on the lower end of the confidence interval, a source that is the owner's words or a
  verified result, and not already covered. Facts about the owner become a proposed `USER.md` edit.
- **Drop:** everything else leaves short-term memory, archived, never deleted.
- Every decision goes to `decisions`; a short morning note says what was kept, dropped and promoted;
  anything can be undone.

**Knowledge.** The wiki holds structured notes: each page an append-only timeline plus a summary
rewritten from it (SPEC.md §5.9). `capture` takes a concept and its source; `search` finds candidate
pages; System One picks the page (existing, new, or a duplicate) and flags unclear or sensitive
content; a model writes the clean text.

**Search.** One index over sessions, memories, the wiki and skills: Postgres full-text first, then
pgvector, merged by rank (reciprocal rank fusion), exact names and paths first. A tool, not injected
every turn. Every search logged.

**Web.** `web_search` behind one provider contract (swappable providers); `web_fetch` with
readable-text extraction and link following; a browser later.

### Skills and tools that improve themselves

**Skills** use the agentskills.io format (`SKILL.md` with frontmatter, `references/`, `scripts/`) in
`~/.zenbot/global/skills/<domain>/<skill>/`, in git. Rules against sprawl (D-029): domains first (a new
domain needs the owner's OK); edit before create (search first; a new skill only when none covers
the work, with the reason recorded); changes from evidence at session close, never from one task;
small and composable; loads and outcomes measured, near-duplicates merged, unused skills retired;
every change a revertible commit.

**MCP client** written against the spec (D-033; built). From FastMCP: namespacing and mounting
(`<server>_<tool>`), tool transformation (rename, hide arguments, rewrite descriptions), middleware
(audit, permissions, secret injection), proxying (zenbot's own tools as an MCP server, SPEC.md §3).

**Agent-made tools** are a script plus a manifest (name, input schema, command), run by the kernel in
the sandbox and kept in git; a full MCP server only when a tool must keep state. A new tool gets no
network or secrets until the owner approves.

**Loading tools mid-session** would change the tool list, which rewrites the cached prefix on every
engine (the Phase 0 spike), so the list stays fixed and `call_tool` runs whatever `load_tool`
showed (built).

### Model choice

Learned from real usage, not fixed tasks (D-030, Phase 6): System One classifies the situation; a
versioned policy maps it to a model; a small share of subtasks explore another model with the choice
probability logged; outcomes (verdicts, corrections, verification, cost) update the policy.
