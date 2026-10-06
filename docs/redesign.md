# Redesign: tools, skills and memory over workflow

Agreed with the owner on 2026-10-06. This is the plan zenbot follows from here; it supersedes the
kernel-enforced workflow of `docs/brief.md` and the old next steps in `SPEC.md` §8.1. Status and the
next step are at the end. Nothing in this document is built yet unless it says so.

## What zenbot is

A maker tool that works like a chief of staff. **The owner drives**: they start every job and decide
where tokens are spent. zenbot never starts a job on its own (for now); inside a job it may split the
work into subtasks and delegate them. Jobs are operational work and building software, across the owner's
job, projects and own companies. zenbot offloads tasks, memory and thinking, so the owner's attention
goes to the frontier of knowledge, gut calls and taste.

**Success is how well zenbot does a job end to end:**

1. identifies the real job to be done (not only the literal request);
2. researches all the context it needs (code, memory, data, the web);
3. decides what to build and what is relevant;
4. brings the owner only the questions and decisions that are theirs;
5. solves it to a state-of-the-art standard.

## Design principle

Give the agent tools, skills and context; don't push it through a workflow. A capable model with the
right tools does better work on open-ended jobs than a fixed sequence of steps (the measured case:
briefs on every job gave the same pass rate, 12/12 → 12/12, at 2.1× the cost). How to frame, brief
and verify becomes knowledge in skills the agent uses when it helps.

Fixed rules exist only for:

| Fixed | Why |
|---|---|
| The owner starts jobs and sets the budget | They drive where tokens go |
| Every action runs through the kernel: sandboxed, secrets masked, audited | Security doesn't depend on the model behaving |
| The tape records everything | What isn't recorded can't be judged or improved |
| The agent never decides the owner's calls | Success step 4; `ask` is how it raises them |

A rule comes back only where measurement shows the agent needs it (e.g. if it often claims "done"
without evidence).

## Engine

- **Provider-free.** Claude Code and Codex on the owner's subscriptions, Pi through OpenRouter, API
  models such as Jev; always the latest models. zenbot owns the skills, tools and context, so any
  model can do any session or subtask. Engine-native features (Claude Code skills, etc.) are not
  used: engines run with their own tools off.
- **Context management stays as built** (`docs/context.md`): the tape is the provider-neutral source
  of truth, engine sessions are a disposable cache, summaries and trimming keep the window lean.
  A model switch re-sends the whole context, so switches happen at natural boundaries (a new session
  or subtask), not on an arbitrary turn.
- **Only what the job needs enters the context.** The prefix (instructions and system tools) is fixed
  for the session; everything loaded later is appended as a tool result, so the cache keeps working.
  Every load is recorded per turn; what is loaded and never used is measured and steered away.

## What the agent gets, and when

| When | What |
|---|---|
| Session start (fixed prefix) | `SOUL.md` · `AGENTS.md` · `USER.md` · `MEMORY.md` (capped) · the system tools with their descriptions · a short index of skill domains · a repo's own `AGENTS.md` as project context |
| On demand (appended) | skills, connected MCP servers' tools, tools the agent made |

The files live in `~/.zenbot/`:

| File | Says | Owner |
|---|---|---|
| `SOUL.md` | who the agent is: character, standards, how it works with the owner | owner; agent proposes |
| `AGENTS.md` | its environment: the VM, its body, where things live, what it can reach | owner; agent proposes |
| `USER.md` | the owner: who they are, their preferences, their context | owner; agent proposes |
| `MEMORY.md` | short-term memory (below) | agent, within a fixed size |

**Who teaches what:** `AGENTS.md` teaches the environment; each tool's own description teaches how
and when to use that tool (what it does, when to use it and when not, what it returns, an example);
skills teach how to do a kind of work well. Tool descriptions are paid on every turn, so they grow
only where the model is measured misusing a tool.

## System tools

Loaded at session start, like the system prompt.

| Tool | What it does | Today |
|---|---|---|
| `bash` | Run commands in the sandbox | exists |
| `read` | Read a file or image | exists |
| `write` | Create or overwrite a file | exists |
| `edit` | Exact string replacement in a file | exists |
| `ask` | Bring the owner 1–3 questions, each with 2–4 options, recommended first; unanswered → the recommendation, recorded as an assumption. Add `wait: false` to keep working on what doesn't depend on the answer | exists (ends the turn) |
| `search` | One search across sessions (this one included), memories and the wiki | new; replaces `history` in Phase 3 |
| `remember` | Add, replace or remove a short-term memory entry, with its source | new |
| `web_search` | Search the web through the configured provider | new |
| `web_fetch` | Fetch a URL as readable text, with its links | new |
| `find_skills` | Search skills by need: names and one-line descriptions | new |
| `load_skill` | Load a skill, or one of its reference files | new |
| `find_tools` | Search MCP and agent-made tools by need: names and one-line descriptions | new |
| `load_tool` | Load a tool's full definition so it can be called | new |
| `decide` | Ask System One typed questions (choice, score, bool), in batches, with probabilities | exists |
| `verify` | A fresh verifier checks work against criteria; it never sees the maker's reasoning | exists inside the brief flow |
| `capture` | Put a concept into the wiki (Phase 4) | new |
| `delegate` | Hand a subtask to a subagent with fresh context and a chosen model (Phase 6) | new |

Going away: `move` (`bash mv`), `propose_brief` (a brief is a file the brief skill writes),
`submit_work` and the other workflow tools, `history` (once `search` exists). Wiki pages, skills and
tool manifests are files, so `write`/`edit` cover authoring; the kernel validates the format on save.

## System One (Jev), used heavily

Fast, cheap typed decisions with probabilities (`s1.decide`, `docs/worker-protocol.md`).

- **Called by the model** through `decide`, taught by its tool description: whenever the answer is
  one of known options, whenever there are many items, whenever a cheap second opinion helps.
  Batches of items, a probability per item, so the model sets its own threshold.
- **Built into tools**, so every engine benefits without asking: `search`, `web_search` and
  `find_skills`/`find_tools` rank and filter by relevance to the stated need; `web_fetch` can keep
  only the parts relevant to the question; `remember`'s sleep hygiene scores entries; `capture`
  routes and de-duplicates.
- **Shadow first.** A new use is logged in `decisions` with its probabilities and, later, what
  actually happened; it acts on its own once its answers match outcomes often enough.
- System One decides (where, whether, which); generative work (writing clean text) goes to a model.

## Memory and knowledge

| Kind | In zenbot | Where |
|---|---|---|
| Working | `MEMORY.md`, fixed size | `memories` table, rendered to the prompt and exported as a file |
| Episodic (what happened) | sessions, the tape | Postgres |
| Semantic (what's true) | long-term memories; the wiki | memories in Postgres (source, supersedes); wiki as markdown in git |
| Procedural (how to do things) | skills, tools | markdown and scripts in git |
| External | `web_search`, `web_fetch` | web content is untrusted input (taint rule, §5.18) |

**Short-term memory.** Anything can be saved to it during the day with `remember`; each entry keeps
its text, source (the owner's words, a verified result, or the agent's inference), when it was made
and when it was last used. `MEMORY.md` is rendered at session start and frozen for the session (the
cache holds; writes show from the next session). Entries may exceed the size during the day up to a
hard ceiling (about 2×), which triggers an immediate tidy-up.

**Sleep hygiene (nightly).** The space is fixed, so entries compete for it. A nightly job, with a
fixed token budget and its cost recorded, asks System One about every entry:

| Question | Type |
|---|---|
| Will this be needed in the coming days? | score |
| Will it still be true in months? | probability |
| Does it change how the agent should act for the owner, across jobs? | score |
| Is it already covered (another memory, a skill, `USER.md`)? | probability |

- **Keep:** rank by likely need, adjusted for recent use; fill the fixed size from the top.
- **Promote to long-term:** only really impactful memories. Durable and impactful at a very high bar
  (e.g. ≥ 0.95) on the **lower end of the confidence interval**, a source that is the owner's words or
  a verified result, and not already covered. Facts about the owner become a proposed `USER.md` edit.
- **Drop:** everything else leaves short-term memory, archived, never deleted (the tape keeps it).
- Every decision goes to `decisions`. A short morning note tells the owner what was kept, dropped
  and promoted, and anything can be undone.

**Knowledge.** The wiki holds structured notes: each page an append-only timeline plus a summary
rewritten from it (`SPEC.md` §5.9). `capture` takes a concept and its source; `search` finds candidate
pages; System One decides the page (existing, new, or a duplicate) and flags unclear or sensitive
content; a model writes the clean text. Skills are knowledge too (how to do things).

**Search.** One index over sessions, memories, the wiki and skills: Postgres full-text first, then
pgvector, merged by rank (reciprocal rank fusion), exact names and paths first. Given to the agent as
a tool, not injected every turn. Every search is logged.

**Web.** `web_search` behind one provider contract (Brave, Exa, Tavily, … swappable); `web_fetch`
with readable-text extraction and link following; a browser later.

## Skills and tools that improve themselves

**Skills** use the agentskills.io format (`SKILL.md` with frontmatter, `references/`, `scripts/`) in
`~/.zenbot/skills/<domain>/<skill>/`, in git. The closed loop must make zenbot better in a domain,
not pile up skills. Hermes is the counter-example: almost every slightly different task created a new
skill, with no categories or composition, so nothing specialized. Rules:

- **Domains first.** A small set (e.g. `build/rust`, `research/web`, `ops/deploy`); a new domain needs
  the owner's OK.
- **Edit before create.** Before writing a skill the agent searches the existing ones; a new skill
  only when none covers the work, with the reason recorded.
- **From evidence, not from one task.** Skills are written or edited at session close, from outcomes
  (the owner's verdict, what failed). A one-off task never makes a skill.
- **Composable.** Small skills that reference each other; detail in `references/`; repeatable steps
  in `scripts/` (deterministic where possible).
- **Measured and pruned.** Each skill records its loads and the verdicts of the sessions that used it;
  a periodic pass merges near-duplicates (found by embedding similarity) and retires unused ones.
- **Versioned.** Every change is a reviewable, revertible commit.

**MCP client** with the official Rust SDK (`rmcp`). From FastMCP: namespacing and mounting (many
servers as one, `<server>_<tool>`), tool transformation (rename, hide arguments, rewrite descriptions,
to fit our conventions and keep descriptions small), middleware (one place for audit, permissions,
secret injection), proxying (zenbot's own tools as an MCP server, `SPEC.md` §3).

**Agent-made tools** are a script plus a manifest (name, input schema, command), run by the kernel in
the sandbox and kept in git; a full MCP server only when a tool must keep state. A new tool gets no
network or secrets until the owner approves.

## Phases

The old Phase 1 (context v2, `docs/context.md`) and Phase 2 (briefs, `docs/brief.md`) are built and
installed. The phases below replace the old next steps.

| Phase | Name | Delivers |
|---|---|---|
| 0 | Ground | Research the reference projects in their code; merge the open fix PRs |
| 1 | Foundation | Prompt files, tool descriptions, short-term memory with sleep, skills, workflow into skills |
| 2 | Reach | `web_search`, `web_fetch`, MCP client, `find_tools`/`load_tool` |
| 3 | Recall | `search` (full-text, then semantic) over sessions and memories; long-term memory acts |
| 4 | Knowledge | The wiki and `capture` |
| 5 | Self-improvement | The agent creates and improves skills and tools, without sprawl |
| 6 | Delegation | `delegate`: subagents, the model chosen per subtask from real usage (below) |

Throughout: System One built into each tool as it lands; every load and call recorded per turn;
harness changes evaluated against the installed version, the owner decides (`evals/README.md`).

### Phase 0: Ground

Check each choice against reference projects **in their code, not their READMEs**, and note in this
document where zenbot follows them and where it goes further (security included):

- Hermes (nousresearch/hermes-agent): SOUL/MEMORY/USER files, frozen memory snapshot, memory caps,
  skill creation and why it sprawls.
- OpenClaw: workspace files (`AGENTS.md`, `SOUL.md`, `USER.md`, `MEMORY.md`, …), how they load.
- agentskills.io and Anthropic's skills: the format, progressive disclosure.
- FastMCP: composition, transformation, middleware, client.
- Letta (MemGPT): memory tiers, eviction, sleep-time compute.
- Anthropic's tool search / deferred tool loading, and Claude Code's deferred tools.
- Voyager: a composable skill library that grows.
- The owner's earlier list for memory and context: gbrain, Karpathy's LLM Wiki, eggshell, qm,
  memvid, goose, caveman.

Merge the open fix PRs (#8, #9, #10, #13) and the docs refresh (#7) independently of this plan.

### Phase 1: Foundation

1. **Prompt files.** `~/.zenbot/` gets `SOUL.md`, `AGENTS.md`, `USER.md`, loaded at session start into
   the session's fixed envelope (`crates/zend/src/compile.rs`, `Envelope`). The text hardcoded in
   `compile::system_prompt` moves into default versions of these files (installed by `install.sh` if
   missing, never overwritten). A repo's `AGENTS.md` still loads as project context. Each file has a
   size cap; sizes are recorded per session.
2. **Tool descriptions.** Rewrite each: what it does, when to use it and when not, what it returns, an
   example. The `<tool_guidelines>` block moves into them. `decide` gets a description that makes it
   used. Measure the prefix's tool-description size.
3. **Short-term memory.** A `memories` table (new, expand-only migration): text, source, kind, state
   (short, long, archived), created and last used, scores. The `remember` tool. `MEMORY.md` rendered
   at session start and frozen; exported to `~/.zenbot/MEMORY.md`. Hard ceiling with an immediate
   tidy-up.
4. **Sleep hygiene.** A nightly systemd timer (like `zen-engines.timer`) starts a kernel job: System One
   scores every entry; keep, drop and promote as above, logged in `decisions`; a morning note (`zen
   status` and the next session). **Promotion runs in shadow mode in Phase 1**: nothing reads
   long-term memory until Phase 3, so its proposals calibrate the threshold first. Fixed nightly
   budget.
5. **Skills.** `~/.zenbot/skills/<domain>/<skill>/SKILL.md` in git; the domain index in the prefix;
   `find_skills` (name and description match, System One ranking when configured) and `load_skill`
   (appended as a tool result). Loads recorded per turn. First skills, written by hand: `brief`
   (framing a job, writing a brief file) and `verify` (when and how to call the `verify` tool).
6. **Workflow out of the kernel.** Remove the gates in `crates/zend/src/flow.rs`: session states,
   the forced frame → approve → work order, `propose_brief`, `submit_work`, approvals, `ZEN_BRIEFS`.
   Keep `verify` (the fresh verifier) and `ask`. Database columns stay (expand-only); `move` goes;
   `history` stays until Phase 3. Update `docs/brief.md`, `docs/client-protocol.md`, the CLI's
   `/brief`, `/go`, `/verify` commands.
7. **Check and ship.** New e2e scenarios: the files load; a skill loads on demand; memory carries
   across sessions; sleep keeps memory within its size. Eval against the installed version (pass
   rate, cost, prefix size); the owner decides. Install and dogfood.

**Done when** every session starts from the four files and the system tools, the agent loads only
the skills it uses, framing and verification happen through skills with no kernel gates, memory
carries across sessions and stays within its size every night, and the eval shows no loss in pass
rate.

### Phase 2: Reach

Starts with a spike: `load_tool` changes the tool list mid-session. Claude Code gets zenbot's tools
through the MCP bridge (`crates/zen-engine/src/bridge.rs`); test whether it picks up a changed list
(MCP `tools/list_changed`) without breaking the cache, and the same for Codex and Pi. Where it can't,
the fallback is a generic `call_tool(name, args)`, which keeps the list fixed at some cost in
reliability. Then `web_search`, `web_fetch`, the MCP client, `find_tools`/`load_tool`.

### Phase 6: model choice from real usage

Deferred, kept so it isn't lost: learn which model fits which kind of work from real sessions, not
fixed eval tasks. System One classifies (did the job or step change, what kind of work, how big);
a versioned policy maps that to a model; a small share of subtasks explore another model, with the
choice probability logged so the comparison stays unbiased (a contextual bandit); outcomes are the
owner's verdicts, corrections, verification and cost. Fixed evals stay as a regression and cost
check. This replaces the old Phase 2b (sweeps, an improver session, simulated usage).

## Open decisions

1. **What System One may see.** Heavy use sends private content (files, search results, wiki text)
   to Jev on OpenRouter, which ends the current rule that tool output never leaves the VM for scoring.
   Recommended: allow it for everything except content marked sensitive (secrets are masked; the main
   model's provider already sees the same content). Not yet decided.
2. **Dogfooding data.** Real use runs on another VM. A `zen export` (sessions, verdicts, cost, model
   choices; secrets masked) would bring it here for evals and, later, Phase 6.

## Status and next step

- 2026-10-06: plan agreed and written down (this document, `SPEC.md` §1, §9, §13). No code yet.
- **Next:** Phase 0, research the reference projects and record the findings here; then Phase 1,
  step 1.
