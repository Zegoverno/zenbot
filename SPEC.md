# zenbot — Specification

> What zenbot is meant to have: its layers, modules and their contracts (the long-term target).
> How it is built and works today, and the agreed near-term design: [DESIGN.md](DESIGN.md). Why:
> [DECISIONS.md](DECISIONS.md). What it's for: [CONTEXT.md](CONTEXT.md). Order of work:
> [ROADMAP.md](ROADMAP.md). Where DESIGN.md's target design (2026-10-06) refines a module below,
> DESIGN.md wins until this file is updated. Section numbers are kept stable; code cites them.

---

## 1. Vision
Moved to [CONTEXT.md](CONTEXT.md).

## 2. Principles
Moved to [CONTEXT.md](CONTEXT.md#principles).

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
- Refined (D-028, DESIGN.md "Memory and knowledge"): a fixed-size short-term `MEMORY.md`; a nightly sleep keeps, drops or promotes; only really impactful memories reach long-term.

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
- **Briefs and verification become a skill and a tool** the agent uses when they help (D-026); the kernel-enforced version below is what's built today and is being removed.
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
Moved to [DESIGN.md](DESIGN.md) (as built) and [MAP.md](MAP.md) (the code). Target layout, not built
yet: crates `zen-exec`, `zen-store`, `zen-mcp`; `packages/web`; `bench/`; `deploy/Caddyfile` and a
sandbox image. Target v1 tables: `projects` · `agents` · `attachments` · `sources` · `wiki_pages`
(index; content in git) · `memory_records` · `taste_records` · `principles` (index) · `chunks` ·
`goals` · `tasks` · `runs` · `evals` · `crons` · `workflows` · `workflow_runs` · `inbox_items` ·
`skills` (index) · `scripts` (index) · `mcp_servers` · `secrets` · `devices` · `bench_runs`.

## 7. Deployment
Moved to [DESIGN.md](DESIGN.md#deployment) and [DEVELOPMENT.md](DEVELOPMENT.md).

## 8. Roadmap
Moved to [ROADMAP.md](ROADMAP.md).

## 9. Success metrics
Moved to [CONTEXT.md](CONTEXT.md#how-success-is-measured).

## 10. Risks
| Risk | Mitigation |
|---|---|
| Building the tool instead of doing the work | Payback rule; pilot project in every milestone; time cap on zenbot work |
| Subscription policies change | Router degrades to API providers; budgets ready |
| Pi / SDK churn | Pin Pi's exact version; a bump is a commit with an eval like any harness change (the daily job only reports a newer Pi). The vendor CLIs (Claude Code, Codex) track their latest versions daily, tested and rolled back on failure (`scripts/update-engines.sh`, `zen-engines.timer`); every turn records the engine version it ran; wrap behind the worker protocol |
| Two languages slow the solo builder | Protocol-first; thin kernel early; generate types |
| Prompt injection / exfiltration | Taint rule, credential injection, approvals, egress allow-lists |
| Knowledge rot (wrong wiki pages compound) | Citations, confidence, lint, human-approved writes |
| Scope creep | Anything not in the current milestone goes to `docs/ideas.md` |

## 11. Open questions
Moved to [ROADMAP.md](ROADMAP.md#open-decisions).

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
Moved to [DECISIONS.md](DECISIONS.md).
