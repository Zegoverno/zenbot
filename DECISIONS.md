# zenbot — Decision Log

> Key choices with their reasons. When you're tempted to revisit something, check here first.
> Newest first. Format: decision, why, what was considered (when recorded), date. A decision changes
> only with the owner's OK; a new entry supersedes an old one and says so, and the old one is marked.

---

## D-040 — zenbot's home is laid out by scope; agents are thin definitions

**Date:** 2026-10-07 · **Status:** accepted (owner) · **Supersedes:** the flat `~/.zenbot` layout (paths in D-027, D-028, D-036, D-037)

**Decision:** Under the zen home, system-wide files stay at the top (`USER.md`, `AGENTS.md`,
`mcp.json`, runtime), each agent's own files go in `agents/<name>/` (only `SOUL.md` today; one agent,
`zenbot`), and knowledge every agent shares goes in `global/` (`MEMORY.md`, `wiki/`, `skills/`,
`tools/`). Projects get `projects/<id>/` with the same folders when the first one exists. The kernel
moves an old flat layout once at startup (before writing defaults, never overwriting) and leaves a
relative symlink at each old path; a later release removes them. Runtime files (`bin`, `env`,
`token`, `engine`, …) stay where they are for now; moving them into `system/` is a separate step.

**Why:** The owner wants the layout ready for more agents now, while it's cheap. Knowledge belongs
to a scope, not to an agent: most of it is about the owner and their projects, so per-agent copies
would drift and every agent would relearn it (Claude Code subagents, the OpenAI Agents SDK and CrewAI
share project context and memory across agents; Letta, which keeps memory per agent, adds shared
blocks for this). The symlinks make a rollback safe: an older build reads the old paths, and without
them it would write a fresh default `SOUL.md` and ignore the owner's.

**Rejected:** a folder per agent holding its own memory, skills and wiki (drift, duplicated
learning); moving runtime files in the same change (the systemd unit, the `zen` link, upgrade and
rollback scripts and the installer all point at them).

## D-039 — Full screen owns the screen: diffed frames, scrolling, a side panel

**Decision:** The full-screen terminal app runs on the alternate screen and keeps the conversation
as entries it re-renders at the current width, in a viewport it scrolls itself (PgUp/PgDn, mouse
wheel). Each frame is composed whole, and only rows that changed are written, overwritten in place.
`/open <path>` shows a file in a side panel next to the chat, reloaded when it changes. `--inline`
keeps the scrollback layout. Supersedes the full-screen part of D-014.

**Why:** The owner saw heavy flicker in the browser terminal: every update erased the live region
and repainted it, and that terminal shows the erase. Diffed rows never blank the screen, the
technique ratatui (Codex) uses. The owner asked for a file beside the chat, which needs the app to
own the whole screen, at the cost of the terminal's native scrollback and selection (`/mouse` gives
selection back).

**Considered:** tmux splits with an `/open` that starts `less` in a pane (no rewrite, but needs tmux
and splits outside zen's layout); ratatui (a new dependency and a rewrite of every renderer; the
diffing is about 100 lines); cell-level diffing (row-level is enough to stop the flicker).

**Date:** 2026-10-07

---

## D-038 — Subagents, and model choice that learns from verdicts

**Decision:** `delegate` runs subagents as child sessions (kind `subagent`) with the parent's
instructions, memory and tools except `ask` and `delegate`; several tasks in one call run at the same
time in the kernel. Without a named model, System One classifies the task's kind of work and the
latest policy version maps it to a model; a small share (ZEN_EXPLORE, 0.1) tries the route's other
candidates, each choice logged in `decisions` with its probability. The evidence is the owner's
verdict on the parent session and cost per subtask. The nightly sleep changes a route only when a
model's Wilson lower bound of acceptance beats the current model's upper bound with at least 20
judged subtasks each; every change is a new version with its reason, and `zen policy undo` reverts.

**Why:** D-030's design, made concrete. Engines run separate tool calls one at a time (measured:
Claude Code ran two `delegate` calls back to back), so parallelism is the kernel's. Logging the
probability of each choice keeps the comparison unbiased (a contextual bandit); bounds on both
sides keep a handful of verdicts from flipping a route.

**Considered:** separate `delegate` calls for parallel work (serialized by the engine), System One
choosing the model directly (it can classify, not judge frontier models), switching on point
estimates (too noisy at this volume).

**Date:** 2026-10-07

---

## D-037 — The workshop: rules the kernel enforces for skills and tools the agent makes

**Decision:** The agent changes skills only through `save_skill` and makes tools only through
`save_tool`. A skill needs a reason (the evidence), fits the agentskills.io format and 10,000
characters, and isn't a near-duplicate of one in its domain (refused with "extend X instead"). New
skills are drafts until the owner accepts them or a session that used them is accepted; a new
domain always needs the owner; changes to active skills apply at once, each a commit. Unused skills
are flagged at 30 days and archived (never deleted) at 90. A made tool is found and called like an
MCP tool, never gets the kernel's environment, and runs with no network and read-only files until
the owner approves it; the approval is in the database, out of the agent's reach.

**Why:** Hermes sprawled because creating a skill was cheaper than improving one, a background fork
was told to save something, and nothing was retired (`docs/research/hermes-openclaw.md`). Voyager
adds a skill only after a critic confirms success; here that's a verified use or the owner. The agent
has a shell, so rules that matter for safety (a tool reaching the network) live where it can't
write.

**Considered:** a background "reflect" fork after busy turns (Hermes; rejected: that's the sprawl), the
owner approving every skill change (too much of the owner's attention), tools as MCP servers (more
moving parts for the same result).

**Date:** 2026-10-07

---

## D-036 — The wiki: pages over timelines; System One routes, the agent writes

**Decision:** Knowledge lives in markdown pages in `~/.zenbot/wiki/` (git), one per concept,
entity, decision, playbook, project or person: frontmatter, a summary rewritten from the timeline,
and an append-only timeline of dated entries with their source. `capture` appends an entry: search
finds candidate pages, System One picks one (or a new page) and says whether the note is already
recorded or sensitive; without System One, an exact title or alias match decides. The kernel keeps
`index.md` and `log.md` and commits; the agent rewrites the summary with `edit`. The wiki isn't
loaded into the instructions: `search` reaches it.

**Why:** gbrain and Karpathy's LLM Wiki converge on summary-over-timeline; the timeline keeps
provenance and never loses an observation, the summary keeps reading cheap. Choosing a page is a
decision (System One); writing a summary is writing (a model). gbrain measured that always loading
pages helped one model and hurt another, so pages are found, not injected
(`docs/research/memory-search-web.md` §2, §C).

**Considered:** pages in Postgres only (not plain files the owner owns), the kernel writing summaries
through a separate model call (another moving part), always loading an index of pages.

**Date:** 2026-10-07

---

## D-035 — Search: three arms in one query; promotion earns its trust

**Decision:** One `search_docs` table over session turns and memories (later the wiki and skills),
indexed in the background. A search runs exact names and paths first (trigram over identifiers),
then full text (`simple` configuration: no stemming, identifiers survive) and meaning (pgvector,
1536-dimension `openai/text-embedding-3-small` through OpenRouter, filled in afterwards) merged by
reciprocal rank fusion (k = 60); System One reranks, an exact hit stays on top. Tool output is not
indexed. Long-term memory is reached only through search. A sleep's promotions are proposals the
owner reviews; promotion acts on its own once the one-sided 95% Wilson lower bound of the owner's
agreement with them reaches 0.95.

**Why:** Full text alone can't find `compile.rs` (Postgres keeps a path as one token) and misses
meaning; vectors alone miss exact names (gbrain's recipe, checked on zenbot's own Postgres,
`docs/research/memory-search-web.md`). A raw System One probability is not a confidence: trust comes
from its track record against the owner's calls, which also makes "shadow until it matches often
enough" one formula.

**Considered:** a separate vector store (another service), stemming (`english` mangles identifiers
and the owner writes in more than one language), promoting on System One's probability alone.

**Date:** 2026-10-07

---

## D-034 — Web access: keyless search by default, untrusted content taints

**Decision:** `web_search` uses Brave or Tavily when their key is set, else a self-hosted SearXNG
started with the service (deploy/compose.yaml, bound to 127.0.0.1), which also rescues one failed
keyed call. `web_fetch` reaches only public addresses: the kernel resolves names itself and keeps
public addresses only, checks IP literals and every redirect, and uses no proxy. Web content (and
the output of remote MCP servers) is wrapped in an envelope a page can't close, and taints the
session; what a tainted session saves to memory counts as inference, so it can never be promoted.

**Why:** Web access must work on a fresh install without buying an API, and web pages are the main
prompt-injection and SSRF risk an agent with a shell faces. Sources: Hermes (url_safety, keyless
rescue), OpenClaw (re-wrap once, redirect checks), Claude Code's WebFetch (`docs/research/`).

**Considered:** DuckDuckGo (unofficial scraping; rejected), OpenRouter's web plugin (paid per call,
answers not results), following redirects freely (SSRF through a public redirector; rejected).

**Date:** 2026-10-07

---

## D-033 — MCP behind three fixed tools; a small client of our own

**Decision:** MCP servers' tools are not added to the model's tool list. The list stays fixed per
session and the model uses `find_tools`, `load_tool` (the schema as a tool result) and `call_tool`.
The client (stdio and streamable HTTP; initialize, tools/list, tools/call) is written in the
kernel against the MCP spec instead of using `rmcp`. Servers are configured in
`~/.zenbot/mcp.json`, the `mcpServers` shape, with `${VAR}` from the environment.

**Why:** The Phase 0 spike showed a changed tool list reaches Claude Code mid-turn but rewrites the
whole cached prefix, and Codex and Pi can't take new tools mid-session at all; a fixed list keeps
the cache on every engine (FastMCP's tool-search transform does the same). `rmcp` would add a
second `reqwest` and a C crypto build for three methods.

**Considered:** Claude Code's own ToolSearch with deferred tools (engine-specific; keeps the cache
only there); `rmcp` (weight); adding MCP tools to the list at session start (every server's tools
in every prompt).

**Date:** 2026-10-07

---

## D-032 — System One may see private content

**Decision:** System One (Jev on OpenRouter) may see private content: file contents, tool output,
memories, search results, not only the owner's messages and final answers. `ZEN_S1_PRIVATE=0`
turns it off, and the tools that would send it more then do without System One.

**Why:** Heavy use of System One inside tools (ranking, filtering, the memory sleep) needs the
material it judges. The main model's provider already sees the same content, and secrets are
masked before anything leaves the kernel. Proposed to the owner with the redesign; they went ahead
with it.

**Considered:** Only web and public content (limits the memory sleep and search ranking); waiting
for a local System One model (delays every System One use in tools).

**Date:** 2026-10-07

---

## D-031 — Docs: one file per kind of fact

**Decision:** The repo's docs are split like this: `CONTEXT.md` (what zenbot is for, success,
principles, constraints), `SPEC.md` (what zenbot is meant to have: modules and their contracts),
`DESIGN.md` (how the system is built and works, and the agreed target design), `MAP.md` (the code:
files, routes, tables, settings), `DEVELOPMENT.md` (how to build, test and ship), `ROADMAP.md`
(phases, the active one, debt, open questions), `PROGRESS.md` (append-only log of what shipped),
`DECISIONS.md` (this file), `AGENTS.md` (rules for agents working on the repo), `README.md` (entry
point, install and use). Deep dives stay in `docs/`.

**Why:** A new session (human or agent) must be able to pick up the work without the conversation
that produced it. One large `SPEC.md` mixed vision, design, status and history, so it went stale in
parts and was expensive to read. The structure follows the one used in the owner's zenfinance repo.

**Considered:** Retiring `SPEC.md` entirely (rejected: it keeps the target modules and their
contracts); renaming `SPEC.md` to `DESIGN.md` (rejected: "what it should be" and "how it is built"
change at different speeds).

**Date:** 2026-10-06

---

## D-030 — Model choice learned from real usage (replaces Phase 2b)

**Decision:** Which model does which work is learned from real sessions, not from sweeps over fixed
eval tasks. System One classifies the situation (did the job or step change, what kind of work, how
big); a versioned policy maps it to a model; a small share of subtasks explore another model, with
the choice probability logged (a contextual bandit); outcomes are the owner's verdicts, corrections,
verification and cost. Switches happen at natural boundaries (new session or subtask), never on an
arbitrary turn. Fixed evals stay as a regression and cost check. Scheduled as Phase 6.

**Why:** Models do best at benchmark-shaped tasks, so fixed tasks say little about real work; real
usage is the only honest signal. A model switch re-sends the whole context, so it must be rare.
System One is good at classifying, not at knowing which frontier model is best: outcomes pick the
model.

**Considered:** Phase 2b's sweeps, improver session and simulated usage (replaced); System One
choosing the model directly (rejected); per-turn switching (rejected: cache cost).

**Date:** 2026-10-06

---

## D-029 — Skills improve in a closed loop, without sprawl

**Decision:** The agent creates and improves its own skills (agentskills.io format, in git) under
rules: skills live under a small set of domains (a new domain needs the owner's OK); edit before
create (search first, a new skill only when none covers the work, with the reason recorded); skills
change at session close from outcomes, never from one task; small and composable (`references/`,
`scripts/`); each skill's loads and the verdicts of sessions that used it are measured; a periodic
pass merges near-duplicates and retires unused skills. The agent can also create and improve tools
(a script plus a manifest, sandboxed; no network or secrets until the owner approves).

**Why:** The goal is to get better within a domain. Hermes is the counter-example: almost every
slightly different task created a new skill, with no categories or composition, so nothing
specialized.

**Date:** 2026-10-06

---

## D-028 — Memory: fixed-size short-term, nightly sleep, a very high bar for long-term

**Decision:** Anything can be saved to short-term memory (`MEMORY.md`, fixed size, rendered at session
start and frozen for the session). A nightly sleep job asks System One about every entry (needed
soon? durable? impactful across jobs? already covered?) and keeps, drops (archived, never deleted) or
promotes. Entries compete for the fixed space. Only really impactful memories reach long-term: very
high probability (e.g. ≥ 0.95) on the lower end of the confidence interval, from the owner's words or
a verified result, not already covered. Facts about the owner become proposed `USER.md` edits.
Promotion runs in shadow mode until long-term memory has a reader (search, Phase 3). Knowledge goes to
a wiki (`capture`, System One routes and de-duplicates); search is full-text then semantic over all of
it; the web through `web_search` (swappable providers) and `web_fetch`.

**Why:** The owner's model: short-term holds what matters now; long-term is earned. A fixed size
forces prioritization. Freezing per session keeps the prompt cache.

**Date:** 2026-10-06

---

## D-027 — Prompt files, system tools, and loading only what's needed

**Decision:** The system prompt comes from `~/.zenbot/SOUL.md` (who the agent is), `AGENTS.md` (its
environment: the VM, its body), `USER.md` (the owner) and `MEMORY.md` (short-term), loaded at session
start. The owner owns SOUL, AGENTS and USER (the agent proposes changes); the agent owns MEMORY. The
system tools load with them: `bash`, `read`, `write`, `edit`, `ask`, `search`, `remember`,
`web_search`, `web_fetch`, `find_skills`, `load_skill`, `find_tools`, `load_tool`, `decide`, `verify`,
`capture`, `delegate`. Skills, MCP servers' tools and agent-made tools load on demand, appended as tool
results so the prefix and the cache hold. Who teaches what: `AGENTS.md` the environment; each tool's
description how and when to use that tool; skills how to do a kind of work. System One is used
heavily: by the model through `decide`, and inside `search`, `web_search`, `find_*`, `web_fetch`,
`remember` and `capture`.

**Why:** Unused tool descriptions and skills cost tokens on every turn and distract the model. Tool
descriptions are the main lever for how well tools are used. Patterns from OpenClaw and Hermes
(workspace files), Anthropic and Claude Code (deferred tools, skills).

**Open:** whether System One may see private content (file contents, search results) on OpenRouter;
recommended: yes, except content marked sensitive. See ROADMAP.md.

**Date:** 2026-10-06

---

## D-026 — Tools and skills instead of a kernel-enforced workflow

**Decision:** Briefs and verification stop being kernel gates and become a `brief` skill and a
`verify` tool the agent uses when they help. The kernel keeps only rules for authority (the owner
starts jobs and sets the budget), safety (every action through the kernel) and measurement (the
tape). A rule comes back only where measurement shows the agent needs it. Supersedes the gates of
D-020.

**Why:** Fixed steps cap quality on open-ended jobs. Measured: briefs on every job gave the same pass
rate (12/12 → 12/12) at 2.1× the cost.

**Date:** 2026-10-06

---

## D-025 — zenbot reframed: a maker tool that works like a chief of staff

**Decision:** zenbot carries the owner's jobs end to end (operational work and building software,
across their job, projects and companies) and offloads tasks, memory and thinking. The owner drives:
they start every job and decide where tokens go; zenbot never starts a job on its own (for now), but
may split a job into subtasks and delegate them. Success is how well it does a job end to end: the
real job identified, the context researched, what to build decided, only the owner's questions
raised, a state-of-the-art result (CONTEXT.md).

**Why:** Stepping back from Engine work (model routing) to what zenbot is for. Most work so far was in
the layer the strategy called commodity; the success measure now points at job quality.

**Considered:** A builder's tool for side-project companies (the earlier vision, kept as the thesis);
an autonomous chief of staff that starts work itself (rejected for now: the owner drives spend).

**Date:** 2026-10-06

---

## D-024 — Branch, pull request, green CI

**Decision:** Every change goes on its own branch and through a pull request; it merges into `main`
only when CI (build, tests, clippy as errors, e2e) passes. CI publishes binaries only for commits that
pass. Harness changes also show the owner an eval before merging.

**Why:** A professional, check-gated flow instead of committing on `main`.

**Date:** 2026-10-06

---

## D-023 — Expand-only migrations, tested and backed up

**Decision:** Migrations only add (tables, columns, indexes); anything dropped, renamed or retyped
goes in a later release than the one that stops using it. `scripts/upgrade.sh` tries new migrations
on a throwaway copy of the live database, and the install backs the live database up first (last 10
kept).

**Why:** A rollback swaps the binaries back but not the schema, so the previous build must keep
working on the newer schema.

**Date:** 2026-10-06

---

## D-022 — Vendor CLIs on their latest versions; Pi pinned

**Decision:** `scripts/update-engines.sh`, daily via `zen-engines.timer`, updates Claude Code and
Codex (vendor release, checksum-verified, installed next to the old one). Each update must pass a
tool-free completion through zen-engine or it is rolled back; no restart, and the job never commits,
builds or restarts zenbot. Pi stays pinned: it is harness code running in our process with the
ChatGPT sign-in, so a bump is a commit with an eval; the job only reports a newer Pi. Results go to
`upgrade.log` and `zen status`; every turn records `engine_version`.

**Why:** Always the latest models and fixes, without an update silently breaking zenbot.

**Date:** 2026-10-06

---

## D-021 — A session is one job; briefs opt-in

**Decision:** The briefed workflow runs once per job, not per message. Briefs are opt-in (`ZEN_BRIEFS`,
default `opt-in`): the model proposes one for a big, risky or unclear job; the owner's `/brief`
forces one. Their value is judged from real use (verdicts and cost, briefed vs unbriefed), not from
synthetic harder evals.

**Why:** The first Phase 2 build re-framed and verified per message (4.9× the cost). With the job
model, briefs on every job still cost 2.1× for the same pass rate.

**Superseded in part by D-026** (briefs become a skill).

**Date:** 2026-10-06

---

## D-020 — Briefed work with kernel-enforced gates

**Decision:** Sessions go frame → approve → work → verify → report → close; the model can run all of
it, the owner can take any step. Gates are kernel-enforced: read-only framing in a bubblewrap
sandbox, brief schema, approval, criteria commands run by the kernel; a fresh verifier never grades
its own work. Auto-approve and auto-close are settings; every verdict records its source. System One
decisions (route, kind of work, model, unverified claims) start in shadow mode. Routing is a
versioned policy: day-to-day changes automatic with a log and undo; system-level changes need the
owner. Phase 2b adds the improvement loop.

**Why:** Studied projects (Superpowers, Spec Kit, GSD, BMAD, Kiro, Claude Code plan mode, Cline, Roo
Code, Codex, Aider) rarely enforce their gates; zenbot runs every tool in the kernel, so it can.

**Superseded by D-026** (gates → skills and tools) **and D-030** (Phase 2b).

**Date:** 2026-10-05

---

## D-019 — Context v2

**Decision:** Instructions fixed per session and stored once (`envelopes`); per-turn context at the end
of the user's message; append-only history; the tape is a hash-linked chain of numbered blocks;
summaries cite block addresses and a `history` tool reads any block back; every turn records what was
sent and any cache break. Engines may take over a step when that measurably gives better results,
behind the worker protocol with a zenbot-owned fallback: Claude Code keeps its own session per zenbot
session (`--resume`) as a cache rebuilt from the tape when needed; Codex gets native history items.
Supersedes "no engine-side sessions" of D-012. See `docs/context.md`.

**Why:** Measured: a fresh Claude Code session per turn never caches earlier turns. With v2, same
model, 11 tasks: 8/8 → 8/8 passed (two long-session tasks newly passing), cost −78%, cache hit
60% → 95%.

**Date:** 2026-10-05

---

## D-018 — Live scoring with System One

**Decision:** The owner's `/done` decision (accept, more, reshape, drop) is the ground-truth label. A
System One model (Jev via OpenRouter, through Pi's classifier API `s1.decide`) answers a versioned
question set per session, from the user's messages and final answers only, never tool output;
answers are stored, not acted on. Open models (Laya, CLM) can take over through the same API once
there are enough labels to calibrate them.

**Date:** 2026-10-02

---

## D-017 — Evals compare harness versions; the owner decides

**Decision:** Fixed tasks in `evals/`; `scripts/eval.sh` runs the previous and the new harness with the
same model on isolated kernels and databases. Before a harness change is merged, the owner sees the
comparison and decides; it is never an automatic gate.

**Date:** 2026-10-02

---

## D-016 — Every turn traced

**Decision:** One `turns` row per turn with what produced it (harness = zenbot build, engine and its
version, model requested and resolved, effort) and its tokens, cost, time and tool errors. Workers
send one message per model call and a turn report.

**Date:** 2026-10-02

---

## D-015 — Full model ids; explicit thinking level

**Decision:** Models are named by their full id, never by an engine's alias. The thinking level
(effort) is a session setting, and the kernel always sends an explicit level, so what ran is known.

**Why:** `opus` moved from Opus 5 to 5.5 with a Claude Code update.

**Date:** 2026-10-02

---

## D-014 — Terminal app layout; instruction files on demand; commit trailers

**Decision:** The terminal app is full screen by default (input pinned to the bottom); `--inline` keeps
the scrollback-following layout. The first time a tool touches a project below the workspace, its
`AGENTS.md`/`CLAUDE.md` is attached to the result and kept in the session's system prompt. Commits
made in a zen session carry `Zen-Session` and `Co-Authored-By` trailers (`scripts/git-hooks`), so each
change links to its transcript.

**Date:** 2026-10-01

---

## D-013 — Robustness pass informed by Pi

**Decision:** The kernel supervises and restarts workers, ends their orphaned turns, cancels its own
tools on abort (process-group kill), stops stalled turns (watchdog) and serializes edits per file.
Tools: partial output on timeout, full output saved when truncated, line-based paging in `read`,
CRLF/BOM-safe edit with a normalized-match fallback. `upgrade.sh` runs a scripted end-to-end turn
(`faux/smoke`) before installing.

**Date:** 2026-10-01

---

## D-012 — Engines: zen-engine drives the official CLIs

**Decision:** The worker is swappable behind `docs/worker-protocol.md`. The default worker `zen-engine`
(Rust) drives the official Claude Code and Codex CLIs on the owner's subscriptions the way qm does:
built-in tools off, zenbot's tools over MCP / dynamic tools, zenbot's system prompt. Pi stays available
as the optional `pi` worker. The kernel routes models to workers. (Its "no engine-side sessions" part
is superseded by D-019.)

**Date:** 2026-10-01

---

## D-011 — Interface: the `zen` terminal app

**Decision:** `zen` is a single Rust binary with a Claude Code/Codex/Pi-style terminal app plus script
commands; the web UI is frozen. zen is used directly, not called from other agents. Supersedes the
"web UI first" part of D-008.

**Date:** 2026-10-01

---

## D-010 — SPEC v0.2: three layers, services first

**Decision:** Three layers (Engine, Mind, Work); Mind and Work built first as services exposed over
MCP; taste as its own module; zen-bench; inbox; taint-based trust model; roadmap reordered (SPEC.md
§3).

**Date:** 2026-09-30

---

## D-009 — Knowledge: our own take

**Decision:** Knowledge is our own design, borrowing from Karpathy's LLM Wiki; an open-source capture
module replaces Readwise later.

**Date:** 2026-09-30

---

## D-008 — Channels

**Decision:** Web UI first, Matrix later. (Web-first superseded by D-011.)

**Date:** 2026-09-30

---

## D-007 — Subscriptions through supported paths

**Decision:** Use the existing Claude and ChatGPT subscriptions through officially supported paths; a
small capped API budget for System One and embeddings.

**Date:** 2026-09-30

---

## D-006 — Postgres + pgvector

**Decision:** Local Postgres with pgvector via `DATABASE_URL`, movable off the VM later.

**Date:** 2026-09-30

---

## D-005 — Rust kernel, TypeScript model worker

**Decision:** Rust kernel (`zend`) + TypeScript model worker (`zen-mind`, using Pi) + TypeScript web
UI.

**Why:** The always-on parts benefit from Rust's safety, footprint and single binary; the LLM
ecosystem (Pi, Claude Agent SDK, MCP SDKs) is TypeScript-first; LLM latency dominates, so the loop's
language doesn't affect speed.

**Date:** 2026-09-30

---

## D-004 — Host-agnostic

**Decision:** zenbot runs on any Linux box; exe.dev is only the current VM.

**Date:** 2026-09-30

---

## D-003 — Composable, open source first

**Decision:** Composable modules with open contracts; adopt existing open source first; every part
switchable.

**Date:** 2026-09-30

---

## D-002 — Developed in public

**Decision:** The repository is public.

**Date:** 2026-09-30

---

## D-001 — A personal tool, not a product

**Decision:** zenbot is a personal tool for getting work done, not a product.

**Date:** 2026-09-30
