# zenbot — Roadmap

> Phases run in order; the active one is marked. Specs for what's next live here; what shipped goes to
> [PROGRESS.md](PROGRESS.md); why, to [DECISIONS.md](DECISIONS.md). The design these phases build is
> in [DESIGN.md](DESIGN.md) ("Target design"). Agreed with the owner on 2026-10-06 (D-025 to D-030).

Rules for every phase: each choice follows the best known practice, checked in reference projects'
code, and says where it comes from (security included); what ships is used on real work the week it
ships; harness changes are evaluated against the installed version (`scripts/eval.sh`) and the owner
decides; every load and call is recorded per turn; System One is built into each tool as it lands.

## Where things stand

Built and installed: the kernel, the `zen` terminal app, the Claude Code / Codex engines
and context v2 (`docs/context.md`). Phase 1 is merged (#17); Phase 2 is built on `feat/reach`. Both
are installed together once Phase 2 merges. See PROGRESS.md. The phases below replace the old next
steps (old "Phase 2b", "Phase 3 memory", "Phase 4 search", "Phase 5 wiki").

| Phase | Name | Delivers | Status |
|---|---|---|---|
| 0 | Ground | Research the reference projects in their code; merge the open fix PRs | done (`docs/research/`) |
| 1 | Foundation | Prompt files, tool descriptions, short-term memory with sleep, skills, workflow into skills | done (#17): 12/12 → 12/12, cost +27% |
| 2 | Reach | `web_search`, `web_fetch`, MCP client, `find_tools` / `load_tool` / `call_tool` | built (#18) |
| 3 | Recall | `search` (exact, full-text and semantic) over sessions and memories; long-term memory acts | done (#19): 12/12, cost −6% |

| 4 | Knowledge | The wiki and `capture` | built |

| 5 | Self-improvement | The agent creates and improves skills and tools, without sprawl | built |
| 6 | Delegation | `delegate`: subagents; model choice from real usage | built |

---

## Phase 0 — Ground `[ done ]`

The reference projects were studied in their code (2026-10-06); the reports, with file paths, are in
`docs/research/`: `hermes-openclaw.md` (prompt files, memory caps, frozen snapshot, skill sprawl),
`skills-tools-mcp.md` (agentskills.io, deferred tools and the cache, FastMCP, rmcp, Voyager; with a
spike on Claude Code), `memory-search-web.md` (Letta, LLM Wiki and gbrain, hybrid search in
Postgres, web search providers, web fetch safety). DESIGN.md's target design cites them. The fix
PRs (#8, #9, #10, #13) were merged on 2026-10-06 without an eval.

## Phase 1 — Foundation `[ done ]`

Merged in #17. Eval against the installed build: 12/12 → 12/12 passed; cost +27% and tokens +37%
on these short tasks (long sessions +16%), cache hit unchanged, time −4%. The extra cost is the
larger fixed prefix (prompt files, memory, skills index, richer tool descriptions: about 7.9k →
16k characters), paid once per session before the cache takes over. Lever if wanted: shorter
descriptions for `verify`, `remember` and `decide`.

## Phase 2 — Reach `[ built ]`

1. **Spike** (`docs/research/skills-tools-mcp.md`): a changed tool list reaches Claude Code
   mid-turn but rewrites the whole cached prefix, on every engine. So the tool list stays fixed:
   `find_tools` (names and one-liners), `load_tool` (the schema, as a tool result) and `call_tool`.
2. `web_search` (Brave or Tavily with a key; SearXNG in `deploy/compose.yaml` without one, which
   also rescues a failed keyed call; System One reranks) and `web_fetch` (readable markdown, public
   addresses only, paging, `focus` through System One, PDFs for pdftotext). Web content is wrapped
   as untrusted and taints the session.
3. MCP client written against the spec (stdio and streamable HTTP; not `rmcp`, D-033), configured
   in `~/.zenbot/mcp.json`.

## Phase 3 — Recall `[ built ]`

- `search` over every session's turns and over short- and long-term memories: an indexer keeps
  `search_docs` current; exact names and paths first (trigram), then full text (`simple` config) and
  meaning (pgvector, `openai/text-embedding-3-small` through OpenRouter) merged by reciprocal rank
  fusion; System One reranks; a memory found counts as used; every search is logged. `history` stays
  for reading messages by number, now in any session.
- Long-term memory is reached through search. Promotions are proposals the owner reviews
  (`zen memory accept|reject`); promotion acts on its own once the Wilson lower bound of the owner's
  agreement with them reaches 0.95 (D-035).

## Phase 4 — Knowledge `[ built ]`

The wiki (`~/.zenbot/global/wiki/`, in git): one page per concept, entity, decision, playbook, project or
person, in gbrain's shape (a summary over an append-only, dated timeline with sources), plus
`index.md` and `log.md` kept by the kernel. `capture` (D-036): search finds candidate pages, System
One picks the page (or a new one) and catches notes already recorded or sensitive, the kernel
appends the entry, masks secrets, labels notes from web-tainted sessions `web`, and commits; the
agent keeps the summary current with `edit`. `search` covers the wiki; the nightly sleep commits the
agent's wiki edits and reports pages without a summary and broken links.

## Phase 5 — Self-improvement `[ built ]`

`save_skill` and `save_tool` (D-037). Skills: the agentskills.io format under 10,000 characters,
a reason (the evidence) required, a near-duplicate in the domain refused with "extend X instead"
(System One, or word overlap), new skills as drafts (`skills/_proposed`) that become active when the
owner accepts them or a session that used them is accepted, a new domain always the owner's call,
every change a commit; the sleep flags skills unused for 30 days and archives them at 90. Tools: a
manifest and files in `~/.zenbot/global/tools/<name>/`, found and called like MCP tools (`made_<name>`),
JSON on stdin, never the kernel's secrets, sandboxed with no network and read-only files until the
owner approves (`made_tools`, out of the agent's reach). A default `work/close` skill says when to
remember, capture and improve skills. `zen skills`, `zen skills accept|reject`, `zen tools
accept|reject`.

## Phase 6 — Delegation and model choice `[ built ]`

`delegate` (D-038): subagents are child sessions with a fresh context and the same instructions,
memory and tools, except `ask` and `delegate`; one call can hand out several `tasks`, which the
kernel runs at the same time (engines may serialize separate tool calls). Taint flows both ways.
Model choice from real usage (D-030): System One classifies the kind of work, the versioned policy
(`zen policy`, `zen policy set|undo`) maps it to a model, a share of subtasks (ZEN_EXPLORE, 0.1)
tries the route's other candidates with the choice probability logged, outcomes are the owner's
verdict on the parent and cost, and the nightly sleep switches a route only when the evidence is
clear (lower bound beats upper bound, 20 judged subtasks each). `ask` takes `wait: false`.

## What's next

The redesign's phases are built. Next: install, dogfood on the other VM, and let real sessions,
verdicts and the owner's reviews (memory promotions, skill drafts, tools, routes) drive what changes.

## Open decisions

1. ~~What System One may see~~: decided (D-032): private content allowed by default,
   `ZEN_S1_PRIVATE=0` turns it off.
2. **Dogfooding data.** Real use runs on another VM. A `zen export` (sessions, verdicts, cost, model
   choices; secrets masked) would bring it here for evals and, later, Phase 6. Not yet scheduled.
3. **Pilot project.** Which side project zenbot serves after zenbot itself.

## Technical debt

- The terminal client was split into focused modules; incremental streaming/transcript rendering,
  reconnect recovery and bounded private prompt history are in place. A future client event contract
  should replace the remaining untyped JSON.
- Settings are read from the environment in ~25 places (no single config).
- Client events are untyped JSON.
- The old workflow's schema stays (expand-only): `sessions.state` and old tape block kinds.
  Drop them in a later release; `policies` is in use by model routing.
- Tools run as the same Unix user as the owner. Shell subprocesses no longer inherit kernel secrets,
  and the verifier hides token files, but an ordinary agent shell can still read the owner's files.
  Strong isolation requires a separate Unix user and a credential the agent cannot read.
- Secret masking's known-value cache is loaded once per kernel start, so newly rotated credentials
  are not known until restart. `~/.zenbot/outputs` needs owner-approved retention/pruning;
  the historical test logs and `/tmp/zend-*` directories are not deleted by the test fix.
- Verify criteria use a read-only bubblewrap shell; future work should centralize all tool
  permissions and taint rules at the dispatch boundary.
- Paths never run with real models: the new tools (`remember`, `load_skill`, `verify`, `ask`) on
  Codex, a model switch mid-session, a failed verification followed by a real fix.

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
