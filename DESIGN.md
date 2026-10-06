# zenbot — Design

> How the system is built and how it works: first as it is today, then the target design agreed on
> 2026-10-06 (D-025 to D-030) that the roadmap builds. What zenbot is meant to have, module by module,
> is in [SPEC.md](SPEC.md); where each piece lives in the code, in [MAP.md](MAP.md); the order of
> work, in [ROADMAP.md](ROADMAP.md). Deep dives: `docs/context.md` (what the model reads each turn),
> `docs/brief.md` (briefed work, being replaced), `docs/worker-protocol.md`,
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
│ API/WS · sessions and tape · context compiler · tools and executor · flow (briefed work)      │
│ System One decisions and scoring · summaries · tracing · worker routing and supervision        │
└────────────┬───────────────────────────────┬─────────────────────────────────────┬───────────┘
             │ JSON-RPC 2.0 over stdio        │ JSON-RPC 2.0 over stdio              │ sqlx
    zen-engine (Rust, default)        zen-mind (TypeScript, optional `pi`)     Postgres + pgvector
    Claude Code CLI ─ MCP bridge ─┐   Pi agent loop, ChatGPT sign-in,          (Docker, deploy/compose.yaml)
    Codex app-server ─ dynamic    │   OpenRouter models, System One
    tools                         └── tools come back to zend
```

| Process | Language | Responsibility |
|---|---|---|
| `zend` | Rust (axum, sqlx, tokio) | Always-on kernel; owns state and side effects; single binary |
| `zen` | Rust | Terminal app (full screen or `--inline`) and script commands (`--json`) |
| `zen-engine` | Rust | Default worker: runs turns on the Claude Code CLI and `codex app-server` on the owner's subscriptions, with zenbot's prompt, tools and history |
| `zen-mind` | TypeScript (Node 22, from source) | Optional worker (`ZEN_WORKERS=engine,pi`): Pi loop, direct ChatGPT sign-in, OpenRouter, System One (`s1.decide`) |
| Postgres | — | 16+ with pgvector; local in compose, movable via `DATABASE_URL` |

The kernel starts each worker as a child process, supervises and restarts it, ends orphaned turns,
and stops stalled turns (watchdog). Each worker lists the models it serves (`models.list`), and the
kernel routes a model to the worker that lists it (`claude/…` and `codex/…` to zen-engine, Pi's
providers to zen-mind). The protocol is in `docs/worker-protocol.md`; the client protocol in
`docs/client-protocol.md`. A scripted model, `faux/smoke` (`ZEN_FAUX=1`), runs turns without a
subscription for tests.

### Sessions, tape and context

Each session has an append-only tape: a chain of blocks with a per-session number (`seq`), a parent
link and a hash over the parent's hash and the content. Block kinds include `message`, `context`,
`envelope`, `compaction`, `engine_session`, and the briefed-work blocks (`state`, `brief`,
`questions`, `submission`, `verification`, …).

What the model reads each turn (`docs/context.md`): the envelope (system prompt and tools, fixed for
the session) → a summary of older turns, if any → the history (append-only) → the new message with
its turn context (the date, and the workflow step when there is one) at the end, sent only when it
changed. The system prompt is zenbot's base instructions
plus instruction files: `~/.zenbot/AGENTS.md` (global), then `AGENTS.md` (or `CLAUDE.md`) from `/`
down to the workspace; a project's file found later by a tool is attached to that tool result and
kept. Each zenbot session keeps a matching Claude Code session (`--resume`) or Codex thread, so
earlier turns come from the provider's cache. Past 70% of the context budget, older turns are
summarized in the background with block addresses; the `history` tool reads any block back.

### Tools today

`bash`, `read`, `write`, `edit`, `move`, `history`, `ask`, `decide`, plus the briefed-work tools
(`propose_brief`, `submit_work`, the verifier's `submit_verdict`). Tool output is cut once, when the
tool runs (full output saved and referenced); edits are serialized per file and CRLF/BOM-safe; tools
are cancelled with their process group on abort.

### Briefed work (being replaced)

Sessions are one job. Briefs are opt-in: framing (read-only, bubblewrap) → brief (schema-checked,
criteria as commands) → approve → work → verify (the kernel runs criteria; a fresh verifier when it
adds) → report → verdict. The kernel enforces the gates in `crates/zend/src/flow.rs`. Phase 1 of the
roadmap removes the gates and keeps `verify` and `ask` (see "Target design"). Details:
`docs/brief.md`.

### System One

A fast typed-decision model (Jev via OpenRouter, through Pi's `s1.decide`): choice, score or bool
questions, answered with probabilities. Used today for live scoring of sessions (from the owner's
messages and final answers only, never tool output; `session_scores`), shadow decisions in briefed
work (route, kind of work, unverified claims; `decisions`), and the model's `decide` tool. Configured
by `ZEN_S1_MODEL` and `OPENROUTER_API_KEY`; needs the `pi` worker.

### Measurement

`turns` (one row per turn), `model_calls`, `tool_calls`, `envelopes`, `session_decisions` (the owner's
`/done` verdicts, with their source), `session_scores`, `decisions`. Evals: `scripts/eval.sh` runs two
harness versions with the same model on isolated kernels and databases (`evals/README.md`).

### Security today

One owner token for the API. Every tool runs in the kernel; framing runs read-only in bubblewrap.
Secrets are masked in tool output; full outputs stay under `~/.zenbot/outputs`. Commands run as the
owner's user on the VM (no per-project sandbox yet). Outward-facing actions are covered by the system
prompt ("ask before"), not enforced.

### Deployment

- `deploy/compose.yaml` runs Postgres (pgvector). `zend` runs on the host as the systemd service
  `zenbot` (installed by `install.sh`) and starts its workers. One port for API, WebSocket and web
  UI; `/health` reports the database, workers and busy sessions.
- `zen-engines.timer` updates the Claude Code and Codex CLIs daily, tested, with rollback; Pi is
  pinned (D-022).
- CI publishes binaries for every commit on `main` that passes its checks; installs and upgrades
  download them or compile. `scripts/upgrade.sh` checks, smoke-tests on a throwaway database copy,
  backs up before migrations, restarts when no session is busy, and rolls back if unhealthy
  (DEVELOPMENT.md).
- `INSTALL.md` is written for a coding agent to follow on a fresh Linux VM.
- Not built yet: `zend` in compose, a reverse proxy with TLS, a sandbox image, nightly off-VM backups.

---

## Target design (agreed 2026-10-06)

Not built yet unless marked. The roadmap builds it in phases; Phase 0 checks each part against
reference projects' code and records the sources here.

### Principle

Give the agent tools, skills and context; don't push it through a workflow (D-026). Fixed rules only
for authority (the owner starts jobs and sets the budget; the agent never decides the owner's calls),
safety (every action through the kernel, sandboxed, secrets masked, audited) and measurement (the
tape). A rule comes back only where measurement shows the agent needs it.

### Engine

- **Provider-free.** Claude Code and Codex on subscriptions, Pi through OpenRouter, API models such as
  Jev; always the latest. zenbot owns skills, tools and context, so any model can do any session or
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
| `ask` | Bring the owner 1–3 questions, each with 2–4 options, recommended first; unanswered → the recommendation, recorded as an assumption. `wait: false` keeps working on what doesn't depend on the answer | exists (ends the turn) |
| `search` | One search across sessions (this one included), memories and the wiki | new; replaces `history` |
| `remember` | Add, replace or remove a short-term memory entry, with its source | new |
| `web_search` | Search the web through the configured provider | new |
| `web_fetch` | Fetch a URL as readable text, with its links | new |
| `find_skills` | Search skills by need: names and one-line descriptions | new |
| `load_skill` | Load a skill, or one of its reference files | new |
| `find_tools` | Search MCP and agent-made tools by need: names and one-line descriptions | new |
| `load_tool` | Load a tool's full definition so it can be called | new |
| `decide` | Ask System One typed questions, in batches, with probabilities | exists |
| `verify` | A fresh verifier checks work against criteria, without the maker's reasoning | exists inside briefed work |
| `capture` | Put a concept into the wiki | new |
| `delegate` | Hand a subtask to a subagent with fresh context and a chosen model | new |

Going away: `move` (`bash mv`), `propose_brief` (a brief is a file the `brief` skill writes),
`submit_work` and the other workflow tools, `history` (once `search` exists). Wiki pages, skills and
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
`~/.zenbot/skills/<domain>/<skill>/`, in git. Rules against sprawl (D-029): domains first (a new
domain needs the owner's OK); edit before create (search first; a new skill only when none covers
the work, with the reason recorded); changes from evidence at session close, never from one task;
small and composable; loads and outcomes measured, near-duplicates merged, unused skills retired;
every change a revertible commit.

**MCP client** with the official Rust SDK (`rmcp`). From FastMCP: namespacing and mounting
(`<server>_<tool>`), tool transformation (rename, hide arguments, rewrite descriptions), middleware
(audit, permissions, secret injection), proxying (zenbot's own tools as an MCP server, SPEC.md §3).

**Agent-made tools** are a script plus a manifest (name, input schema, command), run by the kernel in
the sandbox and kept in git; a full MCP server only when a tool must keep state. A new tool gets no
network or secrets until the owner approves.

**Loading tools mid-session** changes the tool list. Whether each engine picks up a changed list
without breaking the cache (Claude Code through the MCP bridge, Codex, Pi) is tested before Phase 2;
the fallback is a generic `call_tool(name, args)`.

### Model choice

Learned from real usage, not fixed tasks (D-030, Phase 6): System One classifies the situation; a
versioned policy maps it to a model; a small share of subtasks explore another model with the choice
probability logged; outcomes (verdicts, corrections, verification, cost) update the policy.
