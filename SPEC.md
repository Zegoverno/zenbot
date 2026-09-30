# zenbot — Specification

> Status: **draft v0.1** · 2026-09-30 · private
> This document is the source of truth for what zenbot is and how it is built. Change it before changing the architecture.

---

## 1. Vision

zenbot is a **personal + company operating system** for a niche builder: a machine for thinking, analyzing and building, where LLMs are central and agents do the work.

**Thesis.** As AI makes building cheap, value moves to speed, uniqueness and taste. Expect many tiny companies solving very niche problems, often serving agents. Agents won't buy niche *code* (they can regenerate it) — they buy what they can't cheaply regenerate: proprietary or fresh data, precomputed results, access, accountability and judgment. zenbot exists to help one person accumulate that knowledge and taste, and to turn it into working companies.

**zenbot is a tool, not a product.** Its job is to get real work done on side projects (which are the things meant to make money). It is built in public, starting private.

## 2. Principles

1. **Payback first.** Every milestone must be used on real work the week it ships. Build thin vertical slices, not complete modules.
2. **Composable.** Every building block is its own module with a documented contract. Modules talk only through contracts, never through each other's internals.
3. **Adopt before build.** If a good open-source solution exists, plug it in behind our contract. Replace it only when it gets in the way. If a better substitute emerges, switching must be a config change, not a rewrite.
4. **Open contracts.** MCP for tools · OpenAI/Anthropic-compatible HTTP for models · JSON-RPC between kernel and workers · markdown + git for knowledge · Postgres via `DATABASE_URL` for state · S3 API for blobs.
5. **Host-agnostic.** Runs on any Linux box with Docker. No dependency on a specific host's features (exe.dev is only where it runs today).
6. **The kernel owns state; workers are stateless.** All side effects (shell, files, browser, network) go through the kernel, in one place, under one permission model.
7. **Deterministic where possible.** Prompts suggest; code and policies enforce. Over time, repeatable LLM work is distilled into scripts.
8. **Everything is traced.** Every model call, tool call, cost and eval result is recorded and inspectable.

## 3. Glossary

| Term | Meaning |
|---|---|
| **Project** | A workspace for one company / side project: its own wiki, memory, skills, scripts, workflows, agents, crons. Product code lives in *linked* repos, not inside the project. |
| **Global scope** | The layer above all projects: the owner's personal brain, preferences and taste. Projects inherit from it. |
| **Session** | A chat thread with an agent, inside a project. Has an append-only history (the *tape*). Can be archived. |
| **Agent** | A configured persona: instructions, model policy, tools, skills, memory scopes, budget. There is a **main** agent and **specialists**. |
| **Tool** | A capability the model can call: built-in (shell, files, browser), MCP, or a saved script. |
| **Skill** | A how-to: a `SKILL.md` folder with instructions and optional references/scripts, loaded on demand. *Procedural knowledge.* |
| **Script** | One unit of code, one language, typed inputs/outputs, runs once and exits. A function. |
| **Workflow** | A durable composition of steps (scripts, LLM calls, approvals, waits, branches, retries) with persisted state and triggers. Scripts are its steps. |
| **Memory** | Small atomic facts about *how to act*: preferences, conventions, lessons. Auto-extracted, provenance-tracked, always loaded as a compact index. *The sticky notes.* |
| **Wiki** | Durable synthesized knowledge about the world and our domains: full pages with citations, compiled from sources and sessions, read by humans, retrieved on demand. *The library.* |
| **Source** | Raw captured input (URL, PDF, note, email, transcript). Never edited; the wiki is compiled from sources. |
| **Goal** | Why work matters; a measurable outcome. |
| **Task** | A unit of work: owner agent, status, acceptance criteria, parent goal. |
| **Run** | One attempt at a task (or one firing of a cron/workflow), fully traced. |
| **Eval** | A scored judgment of a run against acceptance criteria. |
| **Cron** | A schedule that triggers a script, workflow or agent task. |
| **S2 model** | "System 2" — a large generative LLM for reasoning and writing. |
| **S1 model** | "System 1" — a fast typed decision model (choose / score / yes-no), e.g. Jev, Laya, CLM, or a small LLM used that way. |
| **Embedding model** | Turns text into vectors for semantic search and indexing. |

## 4. Architecture

```
            Web UI (chat · sessions · preview · board · wiki)        [later: Matrix]
                                   │ HTTPS + WebSocket (one port, behind reverse proxy)
┌───────────────────────────────── zend (Rust kernel) ─────────────────────────────────┐
│ API/WS · Auth (device pairing) · Sessions/Tape · Projects/Scopes · Agents · Tasks/Goals│
│ Scheduler · Context manager · Skills registry · MCP hub · Event bus · Tracing          │
│ Executor (shell, PTY, files, background services, containers) · Preview proxy · Browser│
└───────────┬──────────────────────────────┬───────────────────────────────┬────────────┘
            │ JSON-RPC (local socket)      │                               │
     zen-mind (TypeScript)             Postgres + pgvector            Files + git
     agent loop (Pi)                   (state, tape, memory,           (wiki, skills,
     model providers:                   tasks, traces, index)           scripts, workflows
       S2 · S1 · embeddings                                              per project)
     Claude Agent SDK adapter          S3-compatible blob store (attachments, screenshots, backups)
```

### 4.1 Processes

| Process | Language | Responsibility |
|---|---|---|
| `zend` | Rust | Always-on kernel. Owns all state and all side effects. Single binary. |
| `zen-mind` | TypeScript (Node) | Stateless model worker. Runs the agent loop per turn using Pi (`pi-ai`, `pi-agent-core`) and provider adapters. Asks `zend` to execute every tool call. |
| `web` | TypeScript | Web UI, served by `zend` as static assets. |
| `postgres` | — | Postgres 16+ with pgvector. Local in compose; movable via `DATABASE_URL`. |
| sandbox containers | — | Where agent commands and code actually run. |

**Why this split.** The always-on OS parts (process control, files, scheduling, proxying) benefit from Rust's safety, footprint and single-binary deploys. The LLM ecosystem we want to adopt now (Pi's agent loop and 40+ providers including ChatGPT subscription sign-in, the Claude Agent SDK, MCP SDKs) is TypeScript-first. LLM latency dominates, so the language of the loop doesn't affect speed. `zen-mind` can be rewritten in Rust later against the same contract.

### 4.2 Repository layout (target)

```
zenbot/
  SPEC.md  README.md  INSTALL.md  AGENTS.md
  crates/
    zend/            # kernel binary
    zen-proto/       # kernel⇄worker protocol types (source of truth, generates TS types)
    zen-exec/        # executor + sandbox
    zen-store/       # Postgres access, migrations
  packages/
    mind/            # zen-mind worker (TS)
    web/             # web UI (TS)
    proto/           # generated TS protocol types
  deploy/
    compose.yaml  Caddyfile  sandbox/Dockerfile
  docs/adr/          # architecture decision records
```

## 5. Modules and contracts

Each module lists its contract, its default implementation, and what can be plugged in instead.

### 5.1 Sessions (kernel)
- **Does:** create, list, rename, archive, restore, fork sessions; persist the tape.
- **Tape:** append-only event log per session (`message`, `tool_call`, `tool_result`, `model_change`, `compaction`, `context_edit`, `attachment`, `system_delta`). Nothing is deleted; compaction and edits are new entries. Invariant (from qm): *the model reads only what the tape contains, and the tape contains everything the model read.*
- **Borrow:** Pi session tree (id/parentId), qm tape.

### 5.2 Tools & executor (kernel)
- **Built-in tools:** `bash`, `read`, `write`, `edit`, `move`, `list`, `search` (ripgrep), `service.start|stop|logs` (background processes like dev servers), `run` (execute code in a language), `browser.*`, `web.search`, `web.fetch`, `delegate`, `memory.*`, `wiki.*`, `task.*`.
- **Execution:** commands run inside a per-project **sandbox container** with only that project's folders and linked repos mounted. Toolchains via `mise` so any language can run (Python, Node, Go, Rust, Bash, SQL, …).
- **Scripts as tools:** a saved script with a typed signature (parsed from the function signature or a header) automatically becomes a callable tool. *(Windmill's core idea.)*
- **Output handling:** truncate large outputs (e.g. 2000 lines / 50 KB), store the full output as a blob, tell the model where it is.
- **Plug-ins:** Windmill as an alternative executor/workflow engine; gVisor/Firecracker as stronger sandboxes.

### 5.3 Router (worker + kernel policy)
Three model classes behind one interface:

| Class | Used for | Providers (v1) |
|---|---|---|
| **S2** | reasoning, writing, coding, planning | GPT via ChatGPT subscription (Pi OAuth) · Claude via Claude Agent SDK on the Claude subscription (*agent provider*, see below) · any API model via LiteLLM |
| **S1** | routing, triage, relevance scoring, "retry or stop?", "should I act?", eval checks | Jev (API) · Haiku-class small LLM as fallback · later Laya/CLM local |
| **Embeddings** | indexing wiki/memory/sessions, semantic search | API embedding model · later local (Ollama / fastembed) |

- **Two provider kinds.** *Model providers* stream tokens and our loop runs the turn (APIs, ChatGPT subscription, Jev). *Agent providers* run the whole turn themselves while using **our** tools over MCP (Claude Agent SDK on the Claude subscription, `codex app-server`). Subscriptions are used only through officially supported paths.
- **Selection.** Manual: the user picks a model per session or message. Auto: an S1 call classifies the request (e.g. `quick | reasoning | coding | long-context | vision`) and a routing table maps class → model, respecting budgets and availability, with fallback on errors/rate limits.
- **Budgets.** Per agent and per project, in tokens and money; subscription quota awareness so background work can't starve interactive use.
- **Contract:** OpenAI/Anthropic-compatible HTTP for API providers; internal `Provider` interface in `zen-mind`.
- **Plug-ins:** LiteLLM, ngrok AI Gateway, OpenRouter.

### 5.4 Context manager (worker, policy from kernel)
Goal: the model always sees the most relevant, cache-friendly context.
1. **Tiered prompt:** stable (identity, rules, tool index) → project (memory index, skills index) → volatile (retrieved snippets, current state). Keep the stable prefix unchanged for cache hits.
2. **Progressive disclosure:** skills and MCP tool schemas appear as a short index; full content loads on demand (tool search / code-mode for large MCP servers).
3. **Retrieval per turn:** hybrid search (keyword + vector) over memory, wiki and past sessions; an **S1 model scores candidate chunks for relevance** and only top items are injected.
4. **Pruning:** old tool outputs are replaced by stubs pointing to the tape.
5. **Compaction:** near a token threshold, summarize older turns into a structured summary (goal · constraints · done / in progress / blocked · decisions · files · next steps), keeping recent turns verbatim and never splitting tool call/result pairs. Iterative: update the previous summary.
6. **Fresh contexts for subtasks:** delegate to a subagent with only goal + needed context; get back a result, not a transcript.
- **Borrow:** Hermes compressor, Pi compaction/deltas, GSD fresh-context executors.

### 5.5 MCP hub & skills (kernel)
- **MCP:** register servers once (global or per project); credentials resolved by the kernel at call time; tools namespaced `<server>_<tool>`; every call audited. Remote (HTTP + OAuth) and local (stdio, inside the sandbox) servers.
- **Skills:** `SKILL.md` (agentskills.io format) in `global/skills/` and `projects/<p>/skills/`; the model sees name + description; loads the body on demand.
- **Testing:** MCPJam for MCP server evals.

### 5.6 Agents & delegation (kernel)
- **Agent definition** (`agents/<name>.md` with frontmatter): instructions, model policy, allowed tools, skills, memory scopes (read/write), budget, whether it may delegate.
- **Main agent** is the default entry point; **specialists** (e.g. researcher, coder, designer, ops, writer) are invoked by the main agent or by the user.
- **Delegation:** `delegate(agent, goal, context, output_schema?, background?)` creates a child session with a fresh context; the result returns to the parent. Background delegations post back when done and appear on the board. Children cannot widen their parent's permissions; nesting depth is capped.

### 5.7 Projects & scopes (kernel)
- **Scopes:** `global` (owner) → `project:<id>` → `session`/`agent`. Each memory record, wiki page, skill and secret has an owner scope and a sensitivity level (`ordinary | sensitive | private`).
- **Inheritance:** projects read global (except `private`); global never reads project data unless granted. Agents only see scopes listed in their definition.
- **On disk:**
  ```
  data/
    global/   wiki/ memory/ skills/ scripts/ workflows/ agents/ sources/
    projects/<id>/  wiki/ memory/ skills/ scripts/ workflows/ agents/ sources/ project.yaml
  ```
  `project.yaml` lists linked product repos (git URLs + local checkout paths). Each `data/` scope is a git repo.

### 5.8 Knowledge: memory, wiki, sources (kernel + worker)
- **Memory:** records `{id, scope, text, kind (preference|fact|lesson|directive), sensitivity, source_session, created_by, revision}` in Postgres, rendered to markdown for humans. Written explicitly (`memory.remember`) or by post-session extraction (only the user's own statements and verified outcomes; directives quoted verbatim). Loaded as a capped index each session.
- **Wiki:** markdown pages with frontmatter (`type`, `scope`, `sources`, `updated`, `confidence`). Compiled from sources and sessions; answers cite pages; periodic lint (broken links, contradictions, stale claims, gaps). Agent edits go to a **review queue** the owner approves.
- **Sources:** immutable captured inputs with metadata; the raw layer the wiki is built from. (Capture app — the Readwise replacement — is a separate module/project, `zen-capture`, later.)
- **Search index:** chunks + embeddings + full-text in Postgres (pgvector + tsvector), per scope.
- **Our take vs. LLM Wiki:** keep compile-from-sources, citations and lint; add scopes/sensitivity, provenance records, typed pages, hybrid search and human-approved writes.

### 5.9 Tasks, goals, evals — the closed loop (kernel)
Entities: **Goal → Task → Run → Eval**, plus the **Skill / Script / Workflow / Cron** that performed them.

```
Goal ─► Task ─► Run (agent + tools, traced) ─► Eval ─► Review ─► Improve
  ▲                                                               │
  └────────── Skill / Script / Workflow / Cron gets better ◄──────┘
```

- **Board:** kanban view over tasks (`backlog → ready → running → review → done/failed`), filterable by project, goal, agent.
- **Eval:** each task has acceptance criteria; evals are scripts, S1 checks (yes/no, score), or S2 rubric judgments; the owner's accept/reject/edit is the strongest signal.
- **Review:** after runs (and periodically), a reviewer agent with a restricted tool set updates the relevant skill with *lessons, not logs*. Owner approvals and edits are captured as **taste records** (what was chosen, what was rejected, why).
- **Crystallization:** when a task type has succeeded N times, an agent proposes a deterministic script for its repeatable parts; the LLM keeps only the judgment steps. Promotion requires evals to pass.
- **Curator:** unused skills go stale and are archived, never deleted.

### 5.10 Scheduler (kernel)
- **Cron entries** trigger a script, a workflow, or an agent task; per-entry model/budget; explicit run statuses (`ok | failed | skipped | delivery_failed`); silent mode (no notification unless something needs attention).
- **Workflows** v1 are simple: ordered steps with approvals and retries, state in Postgres. Plug-in: Windmill if we outgrow this.
- **First cron:** nightly `pg_dump` + `data/` git push to S3-compatible storage.

### 5.11 Browser & preview (kernel + web)
1. **Web research:** `web.search` (search API: Brave/Exa/Tavily) and `web.fetch` (page → markdown).
2. **Agent browser:** headless Chromium in the sandbox, controlled via CDP; default via an existing MCP server (Playwright MCP or Chrome DevTools MCP): navigate, click, type, read DOM/accessibility tree, console, network, screenshot.
3. **Dev preview:** `service.start` runs a dev server in the sandbox; `zend` proxies its port to `/preview/<session>/<port>`; the web UI shows it in a **preview pane** next to the chat (works from a phone).
4. **Point & comment:** an overlay in the preview lets the user click an element; zenbot captures selector, outer HTML, key computed styles, bounding box and a cropped screenshot, and attaches them to the next message.
5. **Screenshot & annotate:** capture the preview or live browser, draw on it, send as context.
6. **Shared live browser:** stream the agent's browser (CDP screencast) into the UI; the user can watch and take over (e.g. for logins).

### 5.12 Web UI
- Sessions sidebar (per project, archived filter) · chat with streaming, tool-call cards, attachments, model picker (auto / manual) · preview pane · board · wiki browser/editor · review queue (memory/wiki/skill proposals) · traces & costs · settings (models, MCP servers, agents, crons).

### 5.13 Security
- **Auth:** device pairing — a new device requests access; the owner approves from the CLI or an existing session. Sessions use short-lived tokens.
- **Secrets:** encrypted keychain in the kernel; sandboxes never receive raw keys — the kernel injects credentials when proxying tool/model calls. Secret values are masked in outputs.
- **Policy:** per-agent tool allow-lists; hard deny list for destructive commands in every mode; approvals for high-risk actions; outward-facing actions (posting, emailing, publishing, spending) always require owner approval unless explicitly delegated.
- **Isolation:** sandbox containers per project; network egress policy (allow-list for sensitive projects).
- **Supply chain:** skills and MCP servers from outside are reviewed/pinned before use.

### 5.14 Observability
- Every model call: provider, model, class (S2/S1/emb), tokens, cost, latency, cache hits. Every tool call: args, duration, result size, exit status. Stored in Postgres, viewable per session/task/project. Export to Langfuse/OpenTelemetry later.

## 6. Kernel ⇄ worker protocol (sketch)

JSON-RPC 2.0 over a Unix socket. Types are defined once in `crates/zen-proto` and generated for TypeScript.

```
kernel → mind   turn.start   {session_id, agent, model_policy, context: Message[], tools: ToolSpec[], budget}
mind   → kernel turn.event   {session_id, event: text_delta | thinking_delta | tool_call | usage | done | error}
mind   → kernel tool.call    {session_id, call_id, name, args}          (kernel executes, applies policy)
kernel → mind   tool.result  {call_id, content, is_error, details}
kernel → mind   turn.steer   {session_id, message}                      (user message mid-turn)
kernel → mind   turn.abort   {session_id}
kernel → mind   s1.decide    {question_set, state}  → {answers[{value, p}]}
kernel → mind   embed        {texts[], model?}      → {vectors[]}
```

The kernel appends every event to the tape. The worker keeps no state between turns.

## 7. Data model (v1 tables)

`projects` · `agents` · `sessions` · `tape_events` · `attachments` · `tasks` · `goals` · `runs` · `evals` · `crons` · `workflows` · `workflow_runs` · `memory_records` · `wiki_pages` (index; content in git) · `sources` · `chunks` (text, tsvector, embedding, scope) · `taste_records` · `skills` (index; content in git) · `scripts` (index; content in git) · `mcp_servers` · `secrets` (encrypted) · `devices` · `model_calls` · `tool_calls` · `approvals`.

## 8. Deployment

- `deploy/compose.yaml`: `zend` (+ `zen-mind` sidecar), `postgres` (pgvector), `caddy` (reverse proxy, WebSockets, TLS when needed), sandbox image.
- **One public port** behind the reverse proxy; everything (UI, API, WebSocket, previews) goes through it.
- `INSTALL.md` is written **for a coding agent**: any agent on any fresh Linux VM can follow it end to end and finish with a green health check (`/health`).
- Moving the database = changing `DATABASE_URL`. Backups nightly (see 5.10).

## 9. Milestones

Each milestone is a thin slice used on a real side project before the next begins. Target: 1–2 weeks each.

| | Milestone | Scope | Done when |
|---|---|---|---|
| **M0** | Kernel | Repo scaffold; `zend` API/WS; Postgres + migrations; sessions (create/list/archive) + tape; built-in `bash`/`read`/`write`/`edit`/`move`; `zen-mind` with one S2 provider; minimal web chat; device pairing; compose + INSTALL.md + health | I can chat with zenbot from my phone and have it edit files and run commands on the VM |
| **M1** | Build loop | Sandbox containers; `run` in any language (mise); background services; **dev preview pane + point & comment + screenshots**; MCP client; skills | I can build and see a web app with zenbot, pointing at elements to give feedback |
| **M2** | Research & routing | `web.search`/`web.fetch`; agent browser (MCP) + live view; router with S2/S1/embeddings, manual + auto selection, budgets; context manager v1 (pruning, compaction, retrieval) | Long research sessions stay coherent; models are picked automatically |
| **M3** | Projects & knowledge | Projects + scopes; memory records + extraction; wiki compile + lint; review queue; hybrid search | Each side project has its own brain that grows from sessions |
| **M4** | Agents | Agent definitions; main + specialists; `delegate`; board | I hand work to specialists and track it on the board |
| **M5** | Closed loop | Cron (script/workflow/agent); runs + evals; reviewer; taste records; crystallization into scripts | A repeated task measurably improves over a few weeks |
| **M6** | Reach | Matrix channel; publish skill with approval; build-in-public | I use zenbot from Matrix and publish progress with one approval |

## 10. Prior art to study (per module)

| Module | Study |
|---|---|
| Kernel / harness | qm (tape, scopes, harness adapters), Pi (loop, sessions, extensions), Hermes (gateway, learning loop), Codex CLI (Rust core, sandboxing) |
| Executor | Windmill, Vercel Sandbox, NanoClaw (container isolation), ZeroClaw (Rust runtime) |
| Router | LiteLLM, pi-ai, Hermes auxiliary routing, Jev / Laya / CLM |
| Context | Hermes compressor, Pi compaction, GSD, Claude Code skills/subagents |
| Knowledge | gbrain, Karpathy LLM Wiki, LifeOS (TELOS), Sylph, Letta memory |
| Loop | Hermes background review + curator, autoresearch, Stripe Minions blueprints |
| Browser | Playwright MCP, Chrome DevTools MCP, gstack browser |
| Company layer | ClawCompany, Paperclip, Continual Company OS, Buzz |

## 11. Open questions

1. Web UI framework (Svelte / React / Lit)?
2. Sandbox: Docker per project vs. per session; when to move to gVisor?
3. Rust HTTP stack (axum) and Postgres access (sqlx) — confirm.
4. Which search API for `web.search`?
5. Which embedding model first (API vs. local)?
6. Workflow engine: our own minimal one vs. adopting Windmill early?
7. Name and handle for building in public; when to flip the repo public.

## 12. Decision log

| Date | Decision |
|---|---|
| 2026-09-30 | zenbot is a personal tool for getting work done, not a product; built in public (private first). |
| 2026-09-30 | Composable modules with open contracts; adopt existing OSS first; switchable. |
| 2026-09-30 | Host-agnostic; exe.dev is only the current VM. |
| 2026-09-30 | Rust kernel (`zend`) + TypeScript model worker (`zen-mind`, using Pi) + TS web UI. |
| 2026-09-30 | Local Postgres + pgvector via `DATABASE_URL`, movable off-VM later. |
| 2026-09-30 | Use existing Claude and ChatGPT subscriptions through supported paths; small capped API budget for S1 (Jev) and embeddings. |
| 2026-09-30 | Channels: web UI first, Matrix later. |
| 2026-09-30 | Knowledge: our own take (borrowing from LLM Wiki); Readwise to be replaced by an open-source capture module later. |
