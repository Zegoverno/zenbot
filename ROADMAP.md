# zenbot — Roadmap

> Phases run in order; the active one is marked. Specs for what's next live here; what shipped goes to
> [PROGRESS.md](PROGRESS.md); why, to [DECISIONS.md](DECISIONS.md). The design these phases build is
> in [DESIGN.md](DESIGN.md) ("Target design"). Agreed with the owner on 2026-10-06 (D-025 to D-030).

Rules for every phase: each choice follows the best known practice, checked in reference projects'
code, and says where it comes from (security included); what ships is used on real work the week it
ships; harness changes are evaluated against the installed version (`scripts/eval.sh`) and the owner
decides; every load and call is recorded per turn; System One is built into each tool as it lands.

## Where things stand

Built and installed: the kernel, the `zen` terminal app, the Claude Code / Codex engines (and Pi),
context v2 (`docs/context.md`) and briefed work with opt-in briefs (`docs/brief.md`). See
PROGRESS.md. The phases below replace the old next steps (old "Phase 2b", "Phase 3 memory",
"Phase 4 search", "Phase 5 wiki").

| Phase | Name | Delivers | Status |
|---|---|---|---|
| 0 | Ground | Research the reference projects in their code; merge the open fix PRs | **ACTIVE** |
| 1 | Foundation | Prompt files, tool descriptions, short-term memory with sleep, skills, workflow into skills | next |
| 2 | Reach | `web_search`, `web_fetch`, MCP client, `find_tools` / `load_tool` | |
| 3 | Recall | `search` (full-text, then semantic) over sessions and memories; long-term memory acts | |
| 4 | Knowledge | The wiki and `capture` | |
| 5 | Self-improvement | The agent creates and improves skills and tools, without sprawl | |
| 6 | Delegation | `delegate`: subagents; model choice from real usage | |

---

## Phase 0 — Ground `[ ACTIVE ]`

**Goal:** every part of the target design checked against the best reference projects before code.

1. Research, **in their code, not their READMEs**, and record in DESIGN.md where zenbot follows each
   and where it goes further (security included):
   - Hermes (nousresearch/hermes-agent): SOUL/MEMORY/USER files, frozen memory snapshot, memory caps,
     skill creation and why it sprawls.
   - OpenClaw: workspace files (`AGENTS.md`, `SOUL.md`, `USER.md`, `MEMORY.md`, …) and how they load.
   - agentskills.io and Anthropic's skills: the format, progressive disclosure.
   - FastMCP: composition, tool transformation, middleware, client.
   - Letta (MemGPT): memory tiers, eviction, sleep-time compute.
   - Anthropic's tool search / deferred tool loading, and Claude Code's deferred tools.
   - Voyager: a composable skill library that grows.
   - The owner's earlier list for memory and context: gbrain, Karpathy's LLM Wiki, eggshell, qm,
     memvid, goose, caveman.
2. Merge the open fix PRs (#8 kernel robustness, #9 worker turn id, #10 engines fail loudly, #13 kernel
   queries) when green, independently of this plan.

**Done when** DESIGN.md's target design cites its sources and the owner has reviewed it.

---

## Phase 1 — Foundation

**Goal:** sessions start from the prompt files and system tools, load only the skills they use, keep
short-term memory across sessions, and frame and verify through skills instead of kernel gates.

1. **Prompt files.** `~/.zenbot/` gets `SOUL.md`, `AGENTS.md`, `USER.md`, loaded at session start into
   the session's fixed envelope (`crates/zend/src/compile.rs`, `Envelope`). `~/.zenbot/AGENTS.md`
   already loads as a global instruction file; it becomes the environment file. The text hardcoded in
   `compile::system_prompt` moves into default versions of the files (installed by `install.sh` if
   missing, never overwritten). A repo's `AGENTS.md` still loads as project context. Each file has a
   size cap; sizes are recorded per session.
2. **Tool descriptions.** Rewrite each: what it does, when to use it and when not, what it returns, an
   example. The `<tool_guidelines>` block moves into them. `decide` gets a description that makes it
   used. Measure the prefix's tool-description size.
3. **Short-term memory.** A `memories` table (new, expand-only migration): text, source, kind, state
   (short, long, archived), created and last used, scores. The `remember` tool (add, replace, remove).
   `MEMORY.md` rendered at session start and frozen; exported to `~/.zenbot/MEMORY.md`. A hard ceiling
   (about 2× the size) triggers an immediate tidy-up.
4. **Sleep hygiene.** A nightly systemd timer (like `zen-engines.timer`) starts a kernel job: System One
   scores every entry; keep, drop and promote (DESIGN.md "Memory"), logged in `decisions`; a morning
   note in `zen status` and the next session. **Promotion runs in shadow mode**: nothing reads
   long-term memory until Phase 3, so its proposals calibrate the threshold first. Fixed nightly
   budget, cost recorded.
5. **Skills.** `~/.zenbot/skills/<domain>/<skill>/SKILL.md` in git; the domain index in the prefix;
   `find_skills` (name and description match, System One ranking when configured) and `load_skill`
   (appended as a tool result). Loads recorded per turn. First skills, written by hand: `brief`
   (framing a job, writing a brief file) and `verify` (when and how to call the `verify` tool).
6. **Workflow out of the kernel.** Remove the gates in `crates/zend/src/flow.rs`: session states, the
   forced frame → approve → work order, `propose_brief`, `submit_work`, approvals, `ZEN_BRIEFS`. Keep
   `verify` (the fresh verifier) and `ask`. Database columns stay (expand-only); `move` goes; `history`
   stays until Phase 3. Update `docs/brief.md`, `docs/client-protocol.md` and the CLI's `/brief`,
   `/go`, `/quick`, `/verify`.
7. **Check and ship.** New e2e scenarios: the files load; a skill loads on demand; memory carries
   across sessions; sleep keeps memory within its size. Eval against the installed version (pass
   rate, cost, prefix size); the owner decides. Install and dogfood.

**Done when** every session starts from the four files and the system tools, the agent loads only
the skills it uses, framing and verification happen through skills with no kernel gates, memory
carries across sessions and stays within its size every night, and the eval shows no loss in pass
rate.

---

## Phase 2 — Reach

1. **Spike first:** `load_tool` changes the tool list mid-session. Claude Code gets zenbot's tools
   through the MCP bridge (`crates/zen-engine/src/bridge.rs`); test whether it picks up a changed list
   (MCP `tools/list_changed`) without breaking the cache, and the same for Codex and Pi. Where it
   can't, the fallback is a generic `call_tool(name, args)`, which keeps the list fixed at some cost in
   reliability.
2. `web_search` behind one provider contract (Brave, Exa, Tavily, … swappable), results ranked by
   System One; `web_fetch` with readable-text extraction, link following and optional extraction of
   the relevant parts. Web content is marked untrusted (taint rule, SPEC.md §5.18).
3. MCP client (`rmcp`), with namespacing, tool transformation and middleware (audit, permissions,
   secret injection); `find_tools` / `load_tool`.

## Phase 3 — Recall

`search`: Postgres full-text, then pgvector, merged by rank (reciprocal rank fusion), exact names and
paths first, over sessions and memories (later the wiki and skills); System One reranking; every
search logged. Replaces `history`. Long-term memory gets its reader, so promotion leaves shadow mode
once its proposals have matched the owner's calls often enough. Embedding model to choose.

## Phase 4 — Knowledge

The wiki (markdown in git; each page an append-only timeline plus a summary rewritten from it; an
index page) and `capture` (System One picks the page, flags duplicates, unclear or sensitive content;
a model writes the clean text).

## Phase 5 — Self-improvement

The closed loop for skills and tools under D-029's rules: domains, edit before create, changes from
outcomes at session close, measured and pruned, versioned. Agent-made tools (script + manifest,
sandboxed, no network or secrets until the owner approves).

## Phase 6 — Delegation and model choice

`delegate`: subtasks in fresh contexts, each with a chosen model, results back to the parent; `ask`
with `wait: false`. Model choice from real usage (D-030): System One classifies, a versioned policy
maps to a model, a small share of subtasks explore, outcomes decide.

---

## Open decisions

1. **What System One may see.** Heavy use sends private content (files, search results, wiki text) to
   Jev on OpenRouter, ending the rule that tool output never leaves the VM for scoring. Recommended:
   allow everything except content marked sensitive (secrets are masked; the main model's provider
   already sees the same content). Waiting on the owner.
2. **Dogfooding data.** Real use runs on another VM. A `zen export` (sessions, verdicts, cost, model
   choices; secrets masked) would bring it here for evals and, later, Phase 6. Not yet scheduled.
3. **Pilot project.** Which side project zenbot serves after zenbot itself.

## Technical debt

- Settings are read from the environment in ~25 places (no single config).
- Client events are untyped JSON.
- `flow.rs` (~1,000 lines) is mostly removed in Phase 1, step 6.
- WebSocket hubs are never freed; a summary made inline at the hard limit might trip the watchdog
  (both addressed in PR #8).
- The `policies` table is read on approval (`flow::route_for`) but nothing ever writes it, so policy
  routing does nothing until a row exists (found by the MAP.md survey; matters for Phase 6).
- Brief criteria commands run with `bash -lc` outside bubblewrap; instruction files attached to a tool
  result are added after secret masking runs. Both to settle with the sandbox and masking work.
- Stale header comments: `flow.rs` (sessions start `open` by default, not `framing`), `codex.rs`
  (threads persist unless `ZEN_CODEX_RESUME=0`).
- Paths never run with real models: Codex and Pi through the workflow, the questions path, a model
  switch mid-session, a failed verification followed by a real fix.

## Long-term milestones

The original milestones (SPEC v0.2). Each is used on a real side project the week it ships. The
phases above deliver parts of them; what remains is planned after Phase 6.

| | Milestone | Scope | Status |
|---|---|---|---|
| M0 | Walking skeleton | `zend` API/WS, Postgres, sessions and tape, file and shell tools, a model worker, owner token, tracing | done, with the `zen` terminal app instead of the web UI |
| M1 | Mind v0 over MCP | Projects and scopes on disk; sources; wiki and review queue; memory; `taste.record`; full-text search; zenbot MCP server | partly in Phases 1, 3, 4 |
| M2 | Build loop | Sandboxes; `run` in any language; services; dev preview, point & comment, screenshots; MCP client; skills | MCP and skills in Phases 1–2 |
| M3 | Work v0 | Goals, tasks, board; agent definitions; `delegate`; inbox with push; device pairing | `delegate` in Phase 6 |
| M4 | Router, context, bench | System One and embeddings; model selection; budgets; context manager; hybrid search; zen-bench | context done; search in Phase 3; model choice in Phase 6 |
| M5 | Closed loop | Crons; runs and evals; reviewer; taste distillation; crystallization | skills loop in Phase 5 |
| M6 | Research & reach | `web_search` / `web_fetch` with taint rules; agent browser; Matrix; publish with approval | web in Phase 2 |
| M7 | Ship to agents | Project template exposing a project's own MCP server, skills, API and metering | |

## Open questions (long-term)

1. License for the public repo (Apache-2.0, MIT or AGPL-3.0).
2. Web UI framework (Svelte, React, Lit), if the web UI comes back.
3. Sandbox granularity (per project or per session); when to move to gVisor.
4. Search provider for `web_search`; first embedding model.
5. Own minimal workflow engine or Windmill.
6. Public handle and devlog cadence.
