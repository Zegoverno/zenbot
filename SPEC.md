# zenbot — Specification

> Status: **draft v0.2** · 2026-09-30
> This document is the source of truth for what zenbot is and how it is built. Change it before changing the architecture. Significant decisions also get an ADR in `docs/adr/`.

---

## 1. Vision

zenbot is a **personal + company operating system** for a niche builder: a machine for thinking, analyzing and building, where LLMs are central and agents do the work.

**Thesis.** As AI makes building cheap, value moves to speed, uniqueness and taste. Expect many tiny companies solving very niche problems, often serving agents. Agents won't buy niche *code* — they can regenerate it. They buy what they can't cheaply regenerate: proprietary or fresh data, precomputed results, access, accountability and judgment. zenbot exists to help one person **accumulate knowledge and taste, and turn them into working companies**.

**zenbot is a tool, not a product.** Its job is to get real work done on side projects (the things meant to make money). It is developed in public.

## 2. Principles

1. **Payback first.** Every milestone must be used on real work the week it ships. Build thin vertical slices, not complete modules.
2. **Differentiate where it matters, borrow the rest.** Spend effort on the Mind and Work layers (§3). The Engine borrows heavily from existing harnesses.
3. **Composable.** Every building block is a module with a documented contract. Modules talk only through contracts.
4. **Adopt before build.** If a good open-source solution exists, plug it in behind our contract; replace it only when it gets in the way. Switching must be a config change, not a rewrite.
5. **Open contracts.** MCP for tools · OpenAI/Anthropic-compatible HTTP for models · JSON-RPC between kernel and workers · markdown + git for knowledge · Postgres via `DATABASE_URL` for state · S3 API for blobs.
6. **Host-agnostic.** Runs on any Linux box with Docker. exe.dev is only where it runs today.
7. **The kernel owns state; workers are stateless.** All side effects (shell, files, browser, network) go through the kernel, under one permission model.
8. **Deterministic where possible.** Prompts suggest; code and policies enforce. Repeatable LLM work is distilled into scripts over time.
9. **Measured, not assumed.** Every model call, tool call and cost is traced; every claim that zenbot "got better" is backed by `zen-bench` (§5.15).
10. **The owner's attention is the scarcest resource.** Agents interrupt only when needed, through one inbox.
11. **Own your data.** Everything is exportable as plain files; nothing important lives only in a database.

## 3. Strategy: three layers

| Layer | What | Differentiating? | Approach |
|---|---|---|---|
| **Engine** | Sessions, tools, executor, router, context manager, MCP client, skills loader, browser, UI | Mostly commodity | Borrow patterns and code (Pi, qm, Hermes, Codex); keep it lean |
| **Mind** | Sources, wiki, memory, **taste**, search | **Yes: this is the moat** | Our own design |
| **Work** | Projects & scopes, agents, goals/tasks/runs/evals, crons/workflows, inbox, the closed loop | **Yes** | Our own design |

**Services first, exposed over MCP.** Mind and Work are kernel services with an HTTP API **and an MCP server**. That means:
- From week 2, they work inside **Claude Code and Codex**, which already have excellent engines, so zenbot pays back before its own engine is mature.
- zenbot's own engine consumes the same services, so nothing is thrown away.
- Any future harness can plug into zenbot's Mind and Work.

### 3.1 The OS map

| OS concept | zenbot |
|---|---|
| Kernel | `zend`: state, policy, scheduling, side effects |
| Processes | agent runs, visible, killable, with resource limits (budgets) |
| File system | `data/`: global and per-project scopes, git-backed |
| Users & permissions | scopes, sensitivity levels, agent allow-lists |
| Syscalls | the kernel ⇄ worker protocol and the MCP tool surface |
| Drivers | model providers, MCP servers, channels |
| Package manager | skills and MCP servers, pinned and reviewed |
| Shell | chat + command palette (web), later Matrix |
| Notification center | the **inbox** |
| Logs | traces (model calls, tool calls, costs) |
| Cron | scheduler |

## 4. Glossary

| Term | Meaning |
|---|---|
| **Project** | A workspace for one company / side project: its own wiki, memory, taste, skills, scripts, workflows, agents, crons. Product code lives in *linked* repos. |
| **Global scope** | The owner's layer above all projects: personal brain, preferences, taste. Projects inherit from it. |
| **Session** | A chat thread with an agent inside a project, with an append-only history (the *tape*). Can be archived. |
| **Agent** | A configured persona: instructions, model policy, tools, skills, scopes, budget. One **main** agent plus **specialists**. |
| **Tool** | A callable capability: built-in, MCP, or a saved script. |
| **Skill** | A how-to: a `SKILL.md` folder loaded on demand. *Procedural knowledge.* |
| **Script** | One unit of code in one language with typed inputs/outputs; runs once and exits. A function. |
| **Workflow** | A durable composition of steps (scripts, LLM calls, approvals, waits, branches, retries) with persisted state and triggers. |
| **Source** | Raw captured input (URL, PDF, note, email, transcript). Immutable. |
| **Wiki** | Durable synthesized knowledge about the world and our domains: pages with citations, compiled from sources and sessions. *The library.* |
| **Memory** | Small atomic facts about *how to act*: preferences, conventions, lessons. Provenance-tracked, loaded as a compact index. *The sticky notes.* |
| **Taste record** | One captured judgment: what was proposed, what the owner chose/rejected/edited, and why. |
| **Principle** | A distilled, human-approved rule derived from taste records ("prefer X over Y when Z"). |
| **Goal → Task → Run → Eval** | Why → what → one attempt → how good it was. |
| **Inbox item** | Anything waiting on the owner: approval, review, question, failure, digest. |
| **S2 / S1 / Embedding model** | Large generative LLM / fast typed decision model (choose, score, yes/no), e.g. Jev / vector model for search. |
| **Taint** | A session or run that ingested untrusted content (web pages, inbound email, third-party docs). |

## 5. Modules

Each module lists what it does, its contract, the default implementation and possible replacements.

### ENGINE

#### 5.1 Sessions
- Create, list, rename, archive, restore, fork. Append-only **tape** per session: a chain of blocks, each with a per-session number (`seq`, its address), a link to its parent block and a hash over the parent's hash and its content (as git does). Kinds today: `message`, `context`, `envelope`, `compaction`, `engine_session`; later `model_change`, `attachment`, `taint`. See `docs/context.md`.
- Invariant (from qm): *the model reads only what the tape contains, and the tape contains everything the model read.*
- Borrow: Pi session tree, qm tape.

#### 5.2 Tools & executor
- **Built-ins:** `bash`, `read`, `write`, `edit`, `move`, `list`, `search`; `service.start|stop|logs` (background processes such as dev servers); `run` (code in any language); `web.search`, `web.fetch`; `browser.*`; and the Mind/Work tools (`memory.*`, `wiki.*`, `taste.*`, `task.*`, `delegate`, `inbox.ask`).
- Commands run in a **per-project sandbox container** with only that project's `data/` and linked repos mounted. Toolchains via `mise`.
- **Scripts become tools:** a saved script with a typed signature is callable as a tool (Windmill's idea).
- Large outputs are truncated (e.g. 2000 lines / 50 KB); the full output is stored as a blob and referenced.
- Alternatives: Windmill (executor + workflows), gVisor/Firecracker (stronger isolation).

#### 5.3 Router
| Class | Used for | v1 providers |
|---|---|---|
| **S2** | reasoning, writing, coding | GPT via ChatGPT subscription (Pi OAuth) · Claude via Claude Agent SDK on the Claude subscription · API models via LiteLLM |
| **S1** | routing, triage, relevance scoring, "retry or stop?", "notify owner?", eval checks | Jev (API) · small LLM fallback · later Laya/CLM local |
| **Embeddings** | indexing and semantic search | API model first · later local |

- **Two provider kinds.** *Model providers* stream tokens and our loop runs the turn. *Agent providers* (Claude Agent SDK, `codex app-server`) run the whole turn using **our** tools over MCP. Subscriptions are used only through officially supported paths.
- **Selection:** manual per session or message, or automatic: S1 classifies the request → routing table → model, subject to budgets and availability, with fallback on errors and rate limits.
- **Budgets** per agent and project; subscription quota awareness so background work can't starve interactive use.
- **Degradation:** if a subscription path is closed or rate-limited, fall back to API providers per policy.
- Alternatives: LiteLLM, ngrok AI Gateway, OpenRouter.

#### 5.4 Context manager
1. **Tiered prompt:** stable (identity, rules, tool index) → project (memory index, principles, skills index) → history (append-only) → volatile turn context at the end of the user's message (date, retrieved snippets, state). Instructions are fixed per session, so the prompt cache keeps hitting. See `docs/context.md`.
2. **Progressive disclosure:** short indexes for skills and MCP tools; full content on demand (tool search / code-mode for large servers).
3. **Retrieval per turn:** hybrid search over memory, wiki, taste and past sessions; S1 scores candidates; only the top items are injected.
4. **No pruning of sent history:** tool output is cut once, when the tool runs, so what the model saw is what is replayed; full outputs stay readable.
5. **Compaction:** structured summary (goal · state · decisions · files · facts · open · next), each item citing the blocks it came from, updated iteratively; recent turns stay verbatim; tool call/result pairs are never split; prepared in the background and applied after a pause; a `history` tool reads any summarized block back.
6. **Fresh contexts for subtasks** via delegation.
- Every change here is measured per turn (what was sent, cache breaks) and compared with evals before it becomes the default; the owner decides.

#### 5.5 MCP & skills
- MCP client: servers registered globally or per project; credentials resolved by the kernel at call time; tools namespaced `<server>_<tool>`; calls audited.
- Skills: `SKILL.md` (agentskills.io format) in `global/skills/` and `projects/<p>/skills/`, loaded on demand.
- External skills and MCP servers are pinned and reviewed before use.

#### 5.6 Browser & preview
1. `web.search` (search API) and `web.fetch` (page → markdown). **Both taint the session.**
2. Agent browser: headless Chromium in the sandbox via an existing MCP server (Playwright MCP or Chrome DevTools MCP).
3. **Dev preview:** `service.start` runs a dev server; `zend` proxies it to `/preview/<session>/<port>`; the web UI shows it next to the chat.
4. **Point & comment:** click an element in the preview; zenbot attaches its selector, outer HTML, key styles, bounding box and a cropped screenshot to the next message.
5. **Screenshot & annotate.**
6. **Shared live browser:** CDP screencast into the UI; the owner can take over.

#### 5.7 Web UI
- Mobile-friendly (installable PWA, push notifications).
- Views: sessions · chat (streaming, tool cards, attachments, model picker) · preview · **inbox** · board · wiki · traces & costs · settings · command palette.

### MIND

#### 5.8 Sources
- Immutable captured inputs with metadata (`url`, `type`, `captured_at`, `scope`, `hash`); raw layer for the wiki.
- v1 capture: paste/upload in the UI, `sources.add` tool, email-in later. A full capture app (Readwise replacement, `zen-capture`) is a separate project later; study Karakeep, Readeck, Wallabag, Linkwarden, Omnivore first.

#### 5.9 Wiki
- Markdown pages with frontmatter (`type`, `scope`, `sources`, `updated`, `confidence`), typed pages (e.g. `entity`, `concept`, `decision`, `playbook`, `market`), wiki-links.
- Compiled from sources and sessions; answers cite pages; periodic lint (broken links, contradictions, stale claims, gaps).
- Agent edits go to the **review queue** (inbox) unless the owner has granted auto-apply for that scope.
- Our own take: from LLM Wiki we keep compile-from-sources, citations and lint. We add scopes and sensitivity, provenance, typed pages, hybrid search and human-approved writes.

#### 5.10 Memory
- Records `{id, scope, text, kind: preference|fact|lesson|directive, sensitivity, source, created_by, revision}`; rendered to markdown for humans.
- Written explicitly (`memory.remember`) or by post-session extraction. Only the owner's own statements and verified outcomes count; directives are quoted verbatim.
- Loaded as a capped index per session; the rest via search.

#### 5.11 Taste
The core of the moat: learning the owner's judgment and applying it everywhere.
- **Capture signals:** accept/reject of proposals, edits (diff between agent draft and owner's final), rankings between alternatives ("which of these three?"), explicit reasons, and decision-journal entries (`taste.record`).
- **Store:** `taste_records {scope, domain (design|writing|code|product|strategy|…), context, options, choice, diff, reason, created_at}`.
- **Distill:** periodically, an agent proposes **principles** from clusters of records ("prefer plain language over jargon in landing pages"); the owner approves them in the inbox; approved principles are versioned markdown in `global/taste/` or `projects/<p>/taste/`.
- **Apply:** principles are injected as context for matching domains **and** used as eval rubrics (§5.14), so taste both steers and grades.
- **Measure:** track the approval rate without edits per domain over time; it should rise.

#### 5.12 Search
- Chunks + full-text (tsvector) + embeddings (pgvector) in Postgres, per scope; hybrid ranking; optional S1 rerank.
- FTS works on day one; embeddings arrive with the router (M4).

### WORK

#### 5.13 Projects & scopes
- **Scopes:** `global` → `project:<id>` → `session` / `agent`. Every record, page, skill, principle and secret has an owner scope and a sensitivity (`ordinary | sensitive | private`).
- Projects read global (except `private`); global never reads project data unless granted; agents see only the scopes in their definition.
- **On disk (each scope is a git repo):**
  ```
  data/
    global/          wiki/ memory/ taste/ skills/ scripts/ workflows/ agents/ sources/
    projects/<id>/   wiki/ memory/ taste/ skills/ scripts/ workflows/ agents/ sources/ project.yaml
  ```
  `project.yaml` lists linked product repos (git URLs + checkout paths).
- **Project templates:** `zen project new <name> --template <t>` creates the folders, default agents, starter skills and a first goal.

#### 5.14 Agents, tasks and the closed loop
- **Sessions as briefed work** (`docs/brief.md`): frame (read-only, enforced by the kernel) → brief with checkable criteria → approve (owner, or auto-approve per route) → work in a fresh context seeded with the brief → verify (the kernel runs the criteria's commands; a fresh verifier judges the rest) → report → verdict (owner's is the ground truth; the model's is recorded as such). The model can run it end to end; the owner can take any step.
- **Routing policy:** which model and thinking level per kind of work, versioned. Day-to-day policy changes automatically when evals and metrics show a better fit (logged, undoable); system-level changes need the owner.
- **Agent definitions:** `agents/<name>.md` with frontmatter (instructions, model policy, tools, skills, scopes, budget, may-delegate).
- **Delegation:** `delegate(agent, goal, context, output_schema?, background?)` → child session with a fresh context; results return to the parent; background delegations report back through the board and inbox. Children can't widen permissions; nesting depth is capped.
- **Goals → Tasks → Runs → Evals.** Tasks have acceptance criteria. Evals are scripts, S1 checks, S2 rubric judgments (including taste principles), or the owner's verdict, which carries the most weight.
- **Board:** kanban over tasks (`backlog → ready → running → review → done | failed`).
- **Reviewer:** after runs and periodically, an agent with a restricted tool set updates skills with *lessons, not logs*.
- **Crystallization:** when a task type has succeeded N times, an agent proposes a script for its repeatable parts; promotion requires passing evals. The LLM keeps only the judgment steps.
- **Curator:** unused skills go stale and are archived, never deleted.

#### 5.15 zen-bench (evals for zenbot itself)
- 20–30 **real tasks from the owner's work** (research questions, coding changes, writing, ops chores), each with acceptance criteria and a scoring method.
- Runs nightly and on demand against configurations (models, routing tables, context policies, prompts). Reports score, cost and time per task.
- This is how zenbot improves itself: an autoresearch-style loop proposes config changes, runs the bench, and keeps what wins. Promotion needs owner approval.

#### 5.16 Scheduler & workflows
- Cron entries trigger a script, workflow or agent task, each with its own model and budget; explicit statuses (`ok | failed | skipped | delivery_failed`); silent unless an S1 check says the owner should know.
- Workflows v1: ordered steps with approvals, retries and waits; state in Postgres. Adopt Windmill if we outgrow this.
- First crons: nightly backup; nightly `zen-bench`; daily digest to the inbox.

#### 5.17 Inbox
- One queue of everything waiting on the owner: approvals (risky or outward-facing actions, tainted sessions requesting egress), review items (memory, wiki, skill and principle proposals), questions from agents (`inbox.ask`), failures, digests.
- Priority set by rules plus S1; push notifications only above a threshold; batching for the rest.
- Every decision in the inbox is also a taste signal.

### PLATFORM

#### 5.18 Security & trust model
Threats: prompt injection via web/email/docs, secret exfiltration, destructive commands, malicious skills or MCP servers, runaway spend.
- **Auth:** device pairing (new device requests; owner approves from the CLI or an existing session); short-lived session tokens. M0 starts with a single owner token.
- **Secrets:** encrypted keychain in the kernel. Sandboxes never hold raw keys; the kernel injects credentials when proxying. Secret values are masked in outputs.
- **Taint rule:** a session that ingested untrusted content is marked tainted. Tainted sessions can't use secrets, send data out, or take outward-facing actions without an inbox approval. Taint propagates to delegated children.
- **Policy:** per-agent tool allow-lists; a hard deny list for destructive commands; approvals for high-risk actions; outward-facing actions (post, email, publish, spend) always need approval unless explicitly delegated.
- **Isolation:** sandbox per project; egress allow-list for sensitive projects.
- **Spend:** hard budget caps per agent, project and day.
- **Public repo hygiene:** no secrets in git (pre-commit scanning); `data/` is never in the code repo.

#### 5.19 Observability
- Every model call (provider, model, class, tokens, cost, latency, cache hits) and every tool call (args, duration, size, status) is stored in Postgres and viewable per session, task, project and day. OpenTelemetry/Langfuse export later.

## 6. Architecture

```
            Web UI / PWA (chat · inbox · board · preview · wiki)      [later: Matrix]
                                  │ HTTPS + WebSocket (one port, reverse proxy)
┌──────────────────────────────── zend (Rust kernel) ────────────────────────────────┐
│ API/WS · Auth · Policy/Taint · Sessions/Tape · Scheduler · Event bus · Tracing      │
│ Engine: Executor (shell, PTY, files, services, sandboxes) · Preview proxy · MCP hub │
│ Mind:   Sources · Wiki · Memory · Taste · Search                                    │
│ Work:   Projects/Scopes · Agents · Goals/Tasks/Runs/Evals · Crons · Inbox           │
│ Interfaces: HTTP API · MCP server (Mind + Work for Claude Code/Codex/others)        │
└──────────┬────────────────────────────┬─────────────────────────────┬──────────────┘
           │ JSON-RPC (Unix socket)     │                             │
    zen-mind (TypeScript)          Postgres + pgvector            data/ (git)
    agent loop (Pi) · providers    state · tape · index · traces   wiki · memory md · taste
    S2 · S1 · embeddings                                           skills · scripts · workflows
    agent providers (Claude SDK, Codex)   S3-compatible blobs (attachments, screenshots, backups)
```

| Process | Language | Responsibility |
|---|---|---|
| `zend` | Rust (axum, sqlx, tokio) | Always-on kernel; owns state and side effects; single binary |
| `zen-engine` | Rust | Default worker: runs turns on the Claude Code CLI and `codex app-server` (owner's subscriptions) with their own tools off, zenbot's prompt, tools and replayed history |
| `zen-mind` | TypeScript (Node) | Optional worker (`pi`): Pi loop, direct ChatGPT sign-in and API providers |
| `web` | TypeScript | UI, served by `zend` |
| `postgres` | — | 16+ with pgvector; local in compose, movable via `DATABASE_URL` |
| sandboxes | — | Where agent commands and code run |

**Why Rust + TypeScript.** The always-on OS parts benefit from Rust's safety, footprint and single binary. The LLM ecosystem we're adopting (Pi with 40+ providers and ChatGPT sign-in, the Claude Agent SDK, MCP SDKs) is TypeScript-first. LLM latency dominates, so the loop's language doesn't affect speed. `zen-mind` can be ported to Rust later against the same protocol.

### 6.1 Kernel ⇄ worker protocol (sketch)
JSON-RPC 2.0 over a Unix socket; types defined once in `crates/zen-proto` and generated for TypeScript.
```
kernel → mind   turn.start   {session_id, agent, model_policy, context, tools, budget, taint}
mind   → kernel turn.event   {session_id, event: text_delta|thinking_delta|tool_call|usage|done|error}
mind   → kernel tool.call    {session_id, call_id, name, args}      # kernel applies policy, executes
kernel → mind   tool.result  {call_id, content, is_error, details}
kernel → mind   turn.steer | turn.abort
kernel → mind   s1.decide    {questions, state} → {answers: [{value, p}]}
kernel → mind   embed        {texts, model?}    → {vectors}
```

### 6.2 Repository layout
```
zenbot/
  SPEC.md README.md INSTALL.md AGENTS.md LICENSE CONTRIBUTING.md
  crates/   zend/ zen-proto/ zen-exec/ zen-store/ zen-mcp/
  packages/ mind/ web/ proto/
  bench/    tasks/ runner/
  deploy/   compose.yaml Caddyfile sandbox/Dockerfile
  docs/     adr/ devlog/
```

### 6.3 Data model (v1 tables)
`projects` · `agents` · `sessions` · `tape_events` · `attachments` · `sources` · `wiki_pages` (index; content in git) · `memory_records` · `taste_records` · `principles` (index) · `chunks` · `goals` · `tasks` · `runs` · `evals` · `crons` · `workflows` · `workflow_runs` · `inbox_items` · `skills` (index) · `scripts` (index) · `mcp_servers` · `secrets` · `devices` · `model_calls` · `tool_calls` · `bench_runs`.

## 7. Deployment
- `deploy/compose.yaml`: `zend` (+ `zen-mind`), `postgres`, `caddy` (reverse proxy, WebSockets), sandbox image.
- One public port; everything goes through it. `/health` reports each component.
- `INSTALL.md` is written **for a coding agent** to follow on any fresh Linux VM, ending with a green health check.
- Nightly backup: `pg_dump` + push of the `data/` repos to S3-compatible storage. Moving the database = changing `DATABASE_URL`.

## 8. Roadmap

Rules: each milestone is used on a real side project (the **pilot project**) the week it ships. Target is about 1–2 weeks each. Tracing is on from M0.

| | Milestone | Scope | Done when |
|---|---|---|---|
| **M0** | Walking skeleton | Repo, CI, compose; `zend` API/WS; Postgres + migrations; sessions + tape; `bash`/`read`/`write`/`edit`/`move`; `zen-mind` with one S2 provider; minimal web chat; owner token auth; tracing | I chat with zenbot from my phone and it runs commands and edits files on the VM; every call shows its cost |
| **M1** | Mind v0 over MCP | Projects + scopes on disk; sources; wiki pages + review queue; memory records; `taste.record`; full-text search; **zenbot MCP server** | Claude Code / Codex on the VM use zenbot's brain for the pilot project, and the wiki grows from real sessions |
| **M2** | Build loop | Sandboxes; `run` in any language; services; **dev preview + point & comment + screenshots**; MCP client; skills | I build and review the pilot project's web app inside zenbot |
| **M3** | Work v0 | Goals/tasks/board; agent definitions; `delegate`; **inbox** with push; device pairing | I hand tasks to specialists, track them on the board, and approve from my phone |
| **M4** | Router, context, bench | S1 (Jev) and embeddings; auto model selection; budgets; context manager v1; hybrid search; **zen-bench v1** (20 tasks) | Bench runs nightly; auto routing matches or beats my manual picks on cost and quality |
| **M5** | Closed loop | Crons (script/workflow/agent); runs + evals; reviewer; taste distillation into principles; crystallization | One recurring task measurably improves (score up, cost down) over 3 weeks |
| **M6** | Research & reach | `web.search`/`web.fetch` with taint rules; agent browser + live view; Matrix; publish skill with approval | I run research from Matrix and publish a devlog post with one approval |
| **M7** | Ship to agents | Project template exposing the project's own MCP server, skills, API, agent-facing docs and usage metering | A pilot project is usable by other people's agents |

## 9. Success metrics
- Owner hours saved per week (self-reported weekly in the devlog).
- Tasks completed by agents per week, and the share accepted without edits (per domain).
- Cost per accepted task.
- Share of recurring work running as scripts or workflows instead of pure LLM.
- `zen-bench` score and cost trend.
- Wiki pages and principles that the owner actually reads and approves.

## 10. Risks
| Risk | Mitigation |
|---|---|
| Building the tool instead of doing the work | Payback rule; pilot project in every milestone; time cap on zenbot work |
| Subscription policies change | Router degrades to API providers; budgets ready |
| Pi / SDK churn | Pin versions; wrap behind our `Provider` interface |
| Two languages slow the solo builder | Protocol-first; thin kernel early; generate types |
| Prompt injection / exfiltration | Taint rule, credential injection, approvals, egress allow-lists |
| Knowledge rot (wrong wiki pages compound) | Citations, confidence, lint, human-approved writes |
| Scope creep | Anything not in the current milestone goes to `docs/ideas.md` |

## 11. Open questions
1. **License** for the public repo (Apache-2.0 vs MIT vs AGPL-3.0).
2. **Pilot project:** which side project does zenbot serve first?
3. Web UI framework (Svelte / React / Lit).
4. Sandbox granularity (per project vs per session); when to move to gVisor.
5. Search API for `web.search`; first embedding model.
6. Own minimal workflow engine vs adopting Windmill early.
7. Public handle and devlog cadence.

## 12. Prior art (per module)
| Module | Study |
|---|---|
| Kernel / engine | qm (tape, scopes, harness adapters), Pi (loop, sessions, extensions), Hermes (gateway, learning loop), Codex CLI (Rust core, sandboxing) |
| Executor | Windmill, NanoClaw (container isolation), ZeroClaw (Rust runtime), Vercel Sandbox |
| Router | LiteLLM, pi-ai, Hermes auxiliary routing, Jev / Laya / CLM |
| Context | Hermes compressor, Pi compaction, GSD, Claude Code skills and subagents |
| Mind | gbrain, Karpathy's LLM Wiki, LifeOS (TELOS), Sylph, Letta, Khoj |
| Taste | Hermes background review, Sylph approved-vs-draft learning, Impeccable, taste-skill |
| Work / loop | Paperclip, ClawCompany, Stripe Minions blueprints, autoresearch |
| Browser | Playwright MCP, Chrome DevTools MCP, gstack |
| Ship to agents | Continual Company OS, MCP Registry, Cloudflare Monetization Gateway, Stripe metering |

## 13. Decision log
| Date | Decision |
|---|---|
| 2026-09-30 | zenbot is a personal tool for getting work done, not a product; developed in public. |
| 2026-09-30 | Composable modules with open contracts; adopt existing OSS first; switchable. |
| 2026-09-30 | Host-agnostic; exe.dev is only the current VM. |
| 2026-09-30 | Rust kernel (`zend`) + TypeScript model worker (`zen-mind`, using Pi) + TS web UI. |
| 2026-09-30 | Local Postgres + pgvector via `DATABASE_URL`; movable off-VM later. |
| 2026-09-30 | Existing Claude and ChatGPT subscriptions via supported paths; small capped API budget for S1 and embeddings. |
| 2026-09-30 | Channels: web UI first, Matrix later. |
| 2026-09-30 | Knowledge: our own take, borrowing from LLM Wiki; open-source capture module replaces Readwise later. |
| 2026-10-01 | Interface: `zen` single Rust binary with a Claude Code/Codex/Pi-style terminal app plus script commands; web UI frozen. zen is used directly, not called from other agents. |
| 2026-10-01 | Engines: the worker is swappable behind `docs/worker-protocol.md`. Default worker `zen-engine` (Rust) drives the official Claude Code and Codex CLIs on the owner's subscriptions the way qm does: built-in tools off, zenbot's tools over MCP / dynamic tools, zenbot's system prompt, no engine-side sessions (history replayed from the tape). Pi stays available as the optional `pi` worker. The kernel routes models to workers. |
| 2026-10-01 | Robustness pass informed by Pi's code: the kernel supervises and restarts workers, ends their orphaned turns, cancels its own tools on abort (process-group kill), stops stalled turns (watchdog), and serializes edits per file. Tools: partial output on timeout, full output saved when truncated, line-based paging in `read`, CRLF/BOM-safe edit with a normalized-match fallback. AGENTS.md/CLAUDE.md files are loaded into the system prompt. `upgrade.sh` runs a scripted end-to-end turn (`faux/smoke`) before installing. |
| 2026-10-01 | Terminal app: full screen by default (input pinned to the bottom), `--inline` keeps the scrollback-following layout. Instruction files load on demand: the first time a tool touches a project below the workspace, its AGENTS.md/CLAUDE.md is attached to the result and kept in the session's system prompt. Commits made in a zen session carry `Zen-Session` and `Co-Authored-By` trailers (kernel sets `ZEN_SESSION_ID`/`ZEN_MODEL` for commands; `scripts/git-hooks`), so each change links to its transcript. |
| 2026-09-30 | **v0.2:** three layers (Engine / Mind / Work); Mind and Work built first as services exposed over MCP; taste as its own module; zen-bench from M4; inbox; taint-based trust model; roadmap reordered. |
| 2026-10-02 | Models are named by their full id, never by an engine's alias (`opus` moved from Opus 5 to 5.5 with a Claude Code update). The thinking level (effort) is a session setting, and the kernel always sends an explicit level, so what ran is known. |
| 2026-10-02 | Tracing: one `turns` row per turn with what produced it (harness = zenbot build, engine and its version, model requested and resolved, effort) and its tokens, cost, time and tool errors. Workers send one message per model call and a turn report. |
| 2026-10-02 | Evals (zen-bench v0, §5.15): fixed tasks in `evals/`; `scripts/eval.sh` runs the previous and the new harness with the same model on isolated kernels and databases. Before a harness change is committed, the owner sees the comparison and decides; it is never an automatic gate. |
| 2026-10-02 | Live scoring: the owner's `/done` decision (accept, more, reshape, drop) is the ground-truth label. A System One model (Jev via OpenRouter, through Pi's classifier API: `s1.decide`, §6.1) answers a versioned question set per session, from the user's messages and final answers only, never tool output; answers are stored, not acted on. Open models (Laya, CLM) can take over through the same API once there are enough labels to calibrate them. Reports on scores come later (V2). |
| 2026-10-05 | Context v2 (`docs/context.md`): instructions fixed per session and stored once (`envelopes`); per-turn context at the end of the user's message; append-only history; the tape becomes a hash-linked chain of numbered blocks; summaries with block addresses and a `history` tool; every turn records what was sent and any cache break. Engines may take over a step when that measurably gives better results, behind the worker protocol with a zenbot-owned fallback: Claude Code keeps its own session per zenbot session (`--resume`), as a cache rebuilt from the tape when needed (supersedes "no engine-side sessions" of 2026-10-01: measured, a fresh Claude Code session per turn never caches earlier turns); Codex gets native history items. |
| 2026-10-05 | Briefed work (`docs/brief.md`, Phase 2): sessions go frame → approve → work → verify → report → close; the model can run all of it, the owner can take any step. Gates that matter are kernel-enforced (read-only framing in a bubblewrap sandbox, brief schema, approval, criteria commands run by the kernel); a fresh verifier never grades its own work. Auto-approve and auto-close are settings; every verdict records its source. System One decisions (route, kind of work, model, unverified claims) start in shadow mode. Routing is a versioned policy: day-to-day changes are automatic with a log and undo; system-level changes need the owner. Phase 2b adds the improvement loop (sweeps, an improver session, simulated usage). |
