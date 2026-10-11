# zenbot — Roadmap

> Phases run in order, each marked with its status (all are done; "What's next" holds what follows). Specs for what's next live here; what shipped goes to
> [PROGRESS.md](PROGRESS.md); why, to [DECISIONS.md](DECISIONS.md). The design these phases build is
> in [DESIGN.md](DESIGN.md) ("Target design"). Agreed with the owner on 2026-10-06 (D-025 to D-030).

Rules for every phase: each choice follows the best known practice, checked in reference projects'
code, and says where it comes from (security included); what ships is used on real work the week it
ships; harness changes are evaluated against the installed version (`scripts/eval.sh`) and the owner
decides; every load and call is recorded per turn; System One is built into each tool as it lands.

## Where things stand

Every phase below is merged and installed, with what came after (scheduled jobs, the sessions
board, the Matrix channel): `main` is what runs. The kernel, the `zen` terminal app, the Claude
Code / Codex engines (through `zen-engine`; the Pi worker is retired, D-043) and context v2
(`docs/context.md`) were built before. See PROGRESS.md. These phases replaced the old next steps
(old "Phase 2b", "Phase 3 memory", "Phase 4 search", "Phase 5 wiki").

| Phase | Name | Delivers | Status |
|---|---|---|---|
| 0 | Ground | Research the reference projects in their code; merge the open fix PRs | done (`docs/research/`) |
| 1 | Foundation | Prompt files, tool descriptions, short-term memory with sleep, skills, workflow into skills | done (#17): 12/12 → 12/12, cost +27% |
| 2 | Reach | `web_search`, `web_fetch`, MCP client, `find_tools` / `load_tool` / `call_tool` | done (#18) |
| 3 | Recall | `search` (exact, full-text and semantic) over sessions and memories | done (#19): 12/12, cost −6% |
| 4 | Knowledge | The wiki and `capture` | done (#20) |
| 5 | Self-improvement | The agent creates and improves skills and tools, without sprawl | done (#21) |
| 6 | Delegation | `delegate`: subagents; model choice from real usage | done (#22) |

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

## Phase 2 — Reach `[ done ]`

1. **Spike** (`docs/research/skills-tools-mcp.md`): a changed tool list reaches Claude Code
   mid-turn but rewrites the whole cached prefix, on every engine. So the tool list stays fixed:
   `find_tools` (names and one-liners), `load_tool` (the schema, as a tool result) and `call_tool`.
2. `web_search` (Brave or Tavily with a key; SearXNG in `deploy/compose.yaml` without one, which
   also rescues a failed keyed call; System One reranks) and `web_fetch` (readable markdown, public
   addresses only, paging, `focus` through System One, PDFs for pdftotext). Web content is wrapped
   as untrusted and taints the session.
3. MCP client written against the spec (stdio and streamable HTTP; not `rmcp`, D-033), configured
   in `~/.zenbot/mcp.json`.

## Phase 3 — Recall `[ done ]`

- `search` over every session's turns and over memories: an indexer keeps
  `search_docs` current; exact names and paths first (trigram), then full text (`simple` config) and
  meaning (pgvector, `openai/text-embedding-3-small` through OpenRouter) merged by reciprocal rank
  fusion; System One reranks; a memory found counts as used; every search is logged. `history` stays
  for reading messages by number, now in any session.
- Long-term promotion (D-035) was retired by D-045: traits and guidance go to `IDENTITY.md` or
  `USER.md`, which the agent writes and keeps compact.

## Phase 4 — Knowledge `[ done ]`

The wiki (`~/.zenbot/global/wiki/`, in git): one page per concept, entity, decision, playbook, project or
person, in gbrain's shape (a summary over an append-only, dated timeline with sources), plus
`index.md` and `log.md` kept by the kernel. `capture` (D-036): search finds candidate pages, System
One picks the page (or a new one) and catches notes already recorded or sensitive, the kernel
appends the entry, masks secrets, labels notes from web-tainted sessions `web`, and commits; the
agent keeps the summary current with `edit`. `search` covers the wiki; the nightly sleep commits the
agent's wiki edits and reports pages without a summary and broken links.

## Phase 5 — Self-improvement `[ done ]`

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

## Phase 6 — Delegation and model choice `[ done ]`

`delegate` (D-038): subagents are child sessions with a fresh context and the same instructions,
memory and tools, except `ask` and `delegate`; one call can hand out several `tasks`, which the
kernel runs at the same time (engines may serialize separate tool calls). Taint flows both ways.
Model choice from real usage (D-030): System One classifies the kind of work, the versioned policy
(`zen policy`, `zen policy set|undo`) maps it to a model, a share of subtasks (ZEN_EXPLORE, 0.1)
tries the route's other candidates with the choice probability logged, outcomes are the owner's
verdict on the parent and cost, and the nightly sleep switches a route only when the evidence is
clear (lower bound beats upper bound, 20 judged subtasks each). `ask` takes `wait: false`.

## What's next

The redesign's phases are done, and so are scheduled jobs (D-046), session names and suggested
prompts (D-047), the sessions board (D-048) and the Matrix channel (D-049, docs/matrix.md). Next:
keep dogfooding zenbot on real work and let sessions, verdicts and what the owner reviews (the
sleep's promotions into `IDENTITY.md` and `USER.md`, skill drafts, tools, routes) drive what
changes. Candidate follow-ups, each only when use calls for it:

- **Matrix:** interactive (emoji) verification of the bot; streaming answers by editing a message,
  if waiting for the whole answer feels slow.
- **Jobs:** pre-run checks that skip the model when nothing changed, if job costs call for it.
- **Board:** a kernel-wide event stream instead of the 2 s poll, if that feels slow.
- **Suggestions:** improve the suggestion prompt from `zen suggestions` data after a few weeks.

## Open decisions

1. ~~What System One may see~~: decided (D-032): private content allowed by default,
   `ZEN_S1_PRIVATE=0` turns it off.
2. **Pilot project.** Which side project zenbot serves after zenbot itself.

## Technical debt

- Web page/search caches cap at 32 entries each; a page's text caps at 2 MB (5 MB downloaded) and
  entries expire after 15 minutes. The untrusted-content boundary and SSRF filtering remain security-critical.
- The kernel `read` tool refuses files over 16 MiB before loading them; it should eventually stream
  bounded lines so large text files can be paged without `bash`.
- The kernel's tunable settings go through `config.rs` (read once, typed, defaults in one place),
  but these are still read from the environment where they're used: the boot settings in
  `main.rs` (`ZEN_TOKEN`, `ZEN_PORT`, `ZEN_BIND`, `ZEN_WORKSPACE`, `ZEN_REPO`, `ZEN_DEFAULT_MODEL`,
  `ZEN_HARNESS`), the paths (`ZEN_HOME`, `ZEN_SKILLS_DIR`, `ZEN_WIKI_DIR`, `ZEN_TOOLS_DIR`,
  `ZEN_MCP_CONFIG`, read by `layout.rs` and their modules), the workers' (`ZEN_WORKERS`,
  `ZEN_ENGINE_CMD`, `ZEN_WORKER_<NAME>_CMD`, `ZEN_MODELS`) and `ZEN_FAUX`. `zen-engine`, `zen`
  and `zen-matrix` read their own settings directly. Moving the rest into `config.rs` needs the
  tests that set `ZEN_WORKERS` to pass values instead.
- The worker protocol's messages are untyped JSON in `zend` and `zen-engine`
  (`docs/worker-protocol.md`); `zen-proto` types only the client events so far, and the kernel
  still builds those with `json!` rather than from the types.
- The old workflow's schema stays (expand-only): `sessions.state` and old tape block kinds.
  Drop them in a later release; `policies` is in use by model routing.
- Unused since migration 0021 (expand-only): the index `search_docs_unembedded` (replaced by
  `search_docs_to_embed`) and the `memories` and `wiki` keys of `search_state`; drop them in a later
  release. The tape watermark still relies on events being committed within 5 seconds of their id
  (each is its own short transaction, `tape::append`); a queue like the memories' would remove that
  assumption.
- Prompt files (`SOUL`, `IDENTITY`, `AGENTS`, `USER`), active skills and made tools are guarded
  at `edit`/`write` for child and kernel sessions (subagents, verifiers, jobs) and tainted sessions
  (D-045, D-051), and a tainted session's `bash` can't write them (offline sandbox, D-053); the
  owner's config and secrets (`mcp.json`, `env`, `env.prev`, `token`, `db-password`, `matrix*`, `bin/`) for every session. A
  clean session's `bash` can still write all of them. A tainted session's `edit`/`write` can still
  change files outside the workspace that aren't guarded (shell start-up files, other repositories'
  hooks), which a later clean session may run. Unused since D-045: new `memories.tier = 'long'` rows (old ones stay
  searchable) and `sleep_runs.proposed` (always 0); drop them in a later release.
- Tools run as the same Unix user as the owner. Shell subprocesses no longer inherit kernel secrets,
  and the verifier's shell has its own processes, no network and an empty home (D-051), but an
  ordinary agent shell can still read the owner's files, `/proc/<zend pid>/environ` included.
  Strong isolation requires a separate Unix user and a credential the agent cannot read. Worse, on
  the standard install the service user has passwordless `sudo` and Docker access, so an untainted
  session's shell is effectively root. The plan, **not built and needing the owner's yes**: run
  `zend` as its own user without the owner's files; replace blanket `sudo` with a sudoers drop-in
  allowing only `systemd-run … <repo>/scripts/apply-upgrade.sh`, `systemctl restart zenbot` and
  `docker compose -f <repo>/deploy/compose.yaml …`; take the user out of the `docker` group (the
  scripts fall back to `sudo -n docker`, `scripts/db.sh`).
- What each kind of session may call is decided in one place (`agent::refusal`, an allow-list per
  kind), and what a tainted session may do in another (`taint::limited`, D-053), but each tool still
  asks it and applies the answer itself; the shell sandbox is chosen at the dispatcher.
- A draft activated because a session that loaded it was accepted (`workshop::on_accept`) isn't
  tied to the content that session loaded; the owner's own accept is (D-051). Reading a project's
  files with `read` or `bash` isn't taint, only its AGENTS.md is; reading stored tainted content
  (a wiki page with `web` entries, a marked draft) with `bash` (`cat`) isn't either, only with
  `read`, `search`, `history`, `load_skill` and `schedule runs`. The wiki's `index.md` lists a
  page's title even when the title came from untrusted text.
- MCP over HTTP doesn't open the server's event stream, so only stdio servers' `tools/list_changed`
  is seen; an HTTP server's tools are listed again only when it reconnects or `mcp.json` changes. A
  stdio server that times out is started again only when no other call to it is running.
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
| M5 | Closed loop | Crons; runs and evals; reviewer; taste distillation; crystallization | skills loop in Phase 5; crons (scheduled jobs, D-046) built |
| M6 | Research & reach | `web_search` / `web_fetch` with taint rules; agent browser; Matrix; publish with approval | web in Phase 2; Matrix built (D-049) |
| M7 | Ship to agents | Project template exposing a project's own MCP server, skills, API and metering | |

## Open questions (long-term)

1. License for the public repo (Apache-2.0, MIT or AGPL-3.0).
2. Web UI framework (Svelte, React, Lit), if the web UI comes back.
3. Sandbox granularity (per project or per session); when to move to gVisor.
4. Search provider for `web_search`; first embedding model.
5. Own minimal workflow engine or Windmill.
6. Public handle and devlog cadence.
