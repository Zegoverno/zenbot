# zenbot — Decision Log

> The rules that hold today, newest first; check here before revisiting a choice. Full text and
> reasoning: [docs/archive/DECISIONS-full.md](docs/archive/DECISIONS-full.md) (frozen 2026-10-22) and
> [docs/archive/DECISIONS-superseded.md](docs/archive/DECISIONS-superseded.md). A decision changes only
> with the owner's OK. Add new ones at the top in this short form (≤ ~8 lines: status, date, the rule,
> what it supersedes); the full reasoning goes in the pull request and PROGRESS.md.

## D-078 — Free web search: SearXNG natively, no Docker

Status: accepted (owner, 2026-10-22: a free way to search; drop Docker, the old Postgres and the SearXNG container) · 2026-10-22

`scripts/searxng.sh` installs SearXNG from a pinned upstream commit in its own venv as the
`zen-searxng` service on 127.0.0.1:8888 (the kernel's default), with extra engines enabled so one
that blocks doesn't matter. Installs and upgrades set it up; the nightly `engines` job moves it to
upstream's latest commit after a test search, or keeps the current one. Brave or Tavily keys still
take precedence. Docker remains only for migrating a pre-SQLite install.

## D-077 — Clients show only visible text; Matrix upgrades itself; aarch64 releases
Status: accepted · 2026-10-22
The terminal and Matrix drop `zen_proto::invisible` characters. `upgrade.sh` rebuilds a changed zen-matrix and `apply-upgrade.sh` swaps it in, restoring the old one on failure. CI publishes x86_64 and aarch64 builds.

## D-076 — Local SQLite state with verified PostgreSQL migration
Status: accepted · 2026-10-22 · supersedes D-006, D-035's PostgreSQL implementation
State lives in a private, kernel-only SQLite file (WAL, short `BEGIN IMMEDIATE` writes, bundled sqlite-vec and FTS5). PostgreSQL data moves only by a verified, complete snapshot import; never silently roll back to it. Docker is optional; web search uses external SearXNG, Brave or Tavily.

## D-075 — Engines: one cost basis, a concurrency cap, parallel reads, bounded state
Status: accepted · 2026-10-22
One CLI process per turn; costs on one basis (estimated from `prices.rs` when unreported). Background turns wait for an engine slot (`ZEN_ENGINE_CONCURRENCY`, two pools); the owner's never wait. Read-only tools may overlap in a turn. Engine state is pruned after 14 days; a rolled-back CLI update fails the `engines` job.

## D-074 — A learning router picks the model for each piece of work
Status: accepted · 2026-10-22 · supersedes D-066's `unknown` route
Work with no chosen model (owner sessions, agent job runs, subtasks) is routed by System One's `task_kind` through a versioned policy; 5% explores, logged. The owner's choice wins. The nightly `routes` job switches routes on quality-gated evidence (undoable), but only reports a switch to another provider or a model over twice the cost.

## D-073 — Script jobs: a command on a schedule, with no model
Status: accepted · 2026-10-22
A `script` job runs a command in the contained shell (≤ 600 s); its output is the rated, masked report, with command and exit code recorded. One the agent creates or changes stays paused until the owner resumes it; the kernel won't run it unapproved.

## D-072 — MCP sign-in from the terminal: the owner pastes back the redirect address
Status: accepted · 2026-10-22
`zen mcp-login` / `/mcp login` run the MCP authorization flow (PKCE, state and issuer checked); the owner approves in any browser and pastes back the final address. Only the owner's token drives it, never the agent or Matrix; credentials are bound to the server URL.

## D-071 — Phase 5 tools: rich results, batch edits, bounded output, a fuller MCP client
Status: accepted; browser sign-in added by D-072 · 2026-10-21
Results may carry kernel-hashed images; `read` shows images and PDF pages; `edit` applies up to 100 replacements atomically. Output past 12 KB shows head and tail, the full masked text saved with its rating. MCP calls connect only their server; elicitation waits for the owner's answer. Tool descriptions stay under 16,000 bytes.

## D-070 — Migrate legacy knowledge as sources; retire old writers and made tools
Status: accepted · 2026-10-21 · supersedes D-036, D-037's made tools
Legacy wiki pages become provisional source records, nothing lost. `capture` and made tools are retired; repeatable procedures go in reviewed skill folders with scripts.

## D-069 — Three curated stores in Markdown; search is a tool, guided by System One
Status: accepted · 2026-10-21 · supersedes D-028, D-045's sleep promotion, D-037's activation by session
Memory, knowledge and skills are Markdown files (the database is a rebuilt index). Short-term memory is a capped `MEMORY.md`; long-term is earned, mostly via the sleep, which never touches prompt files. Nothing is pushed into a turn: `search` retrieves, System One ranks. Records carry their writers' ratings; skill changes activate only on the owner's review.

## D-068 — Approved job authority and recoverable kernel work
Status: accepted · 2026-10-19
Owner approval of a job holds only for its unchanged definition; an agent edit revokes it. Held runs wait up to two hours. MCP stdio servers are contained. Busy prompts persist until consumed; steering stops at a tool boundary.

## D-067 — Security: context risk, action alignment, contained sessions
Status: accepted · 2026-10-16 · supersedes D-063, parts of D-056, D-062, D-065
One S1 decision file (`action_gate.toml`) decides every effect from context risk (owner's trust list, else System One's source rating, raise-only scan), action risk (local, persistent, outward, irreversible) and, when needed, alignment with the owner's goal (never reading tool output): run, hold or lock. Every shell is sandboxed with network only via the kernel's egress proxy; acting as the owner is a gated `host: true` command.

## D-066 — S1: every System One decision is a file, decided by its costs, recorded and learned from
Status: accepted; `unknown` route superseded by D-074 · 2026-10-16 · supersedes D-038's classifier, session scoring, `decide`
One engine (`crates/zend/src/s1/`) makes every System One call. Decisions are TOML files (owner overrides in `~/.zenbot/s1/`) stating mistake costs, not thresholds; code fixes what each sees and allows; security decisions fail toward caution. Every call is recorded. The brain asks via `judge`: information, never approval.

## D-065 — Metadata source judgments, authenticated job checks and owner queue
Status: partly superseded by D-067 · 2026-10-16
System One lowers a source rating only from metadata, never page text; content checks only raise. Only owner allowlist entries authorize unseen egress. One owner-only queue decides held effects and source proposals (approve once, reject, deny); Matrix reactions never authorize.

## D-064 — Items rated by origin; deny and trust by cause; shells see contaminated files masked
Status: accepted · 2026-10-16 · supersedes parts of D-053, D-054
Stored items carry their writer's risk by exact bytes; only 0–1 enter instructions, others are handed over wrapped and raise the reader. The owner's `/deny` contaminates by cause, `/trust` lifts reviewed causes; no agent tool can. Shells see contaminated files masked.

## D-063 — Project code defaults to offline and read-only
Status: superseded by D-067 (now its high-risk shell mode) · 2026-10-15

## D-062 — Risk is rated per block by its source; a reversible taint stops the session
Status: partly superseded by D-064, D-067 · 2026-10-15
Every block and stored item has a risk 0–5 by source (0 owner, 1 internal, 2 owner-allowlisted, 3 owner-rated, 4 open/unknown, 5 flagged); content checks only raise it. The allowlist changes only by the owner's commands.

## D-061 — Lighter docs and checks
Status: accepted · 2026-10-14
A change updates PROGRESS.md, DECISIONS.md, MAP.md, README.md and the module header as needed; other docs are fixed when wrong; old history moves to `docs/archive/`. `scripts/check-docs.py` checks drift and line budgets.

## D-060 — A job's report can be posted into one of the owner's sessions
Status: accepted · 2026-10-14
A job's `post_to` appends an `ok` run's report to one of the owner's sessions, once, unless the run read untrusted content; everything else goes to the **zen jobs** room.

## D-059 — Bounded disk use: prune our own leftovers, warn at 80%
Status: accepted · 2026-10-14
zen prunes only what it makes (dumps beyond 3, eval runs over 7 days, large build dirs, Docker leftovers); `zen status` warns over 80% disk.

## D-058 — After a usage-limit failover, the session keeps the new model
Status: accepted · 2026-10-14 · amends D-044
After a usage-limit failover the session keeps the new model until the owner's `/model`.

## D-057 — A job run's session is archived when the run ends
Status: accepted · 2026-10-14 · amends D-048
A job run's session is archived when the run ends, however it ends.

## D-056 — Judge the action, not the reading; first step: search doesn't spread the mark
Status: partly superseded by D-062, D-067 · 2026-10-13
Reading costs little; actions are checked. `search` lists marked results without their text until asked by name. A worktree of a trusted repository is trusted.

## D-055 — A job's report can come from a file the run wrote, not the final answer
Status: accepted · 2026-10-13
A `report_file` must be a regular, non-secret workspace file written during the run, UTF-8, ≤ 60 KB; otherwise the run errors, never falling back to the answer.

## D-054 — The owner can trust a session again (`/trust`), forward only
Status: partly superseded by D-064 · 2026-10-13
Only the owner lifts a session's mark (`/trust`), after seeing what set it, forward only.

## D-053 — Taint is a property of stored content; a tainted session's shell has no network
Status: partly superseded by D-062, D-064, D-067 · 2026-10-11
Stored content carries its writer's mark (now rating) and reaches sessions only through the kernel's hand-over path, never as trusted instructions.

## D-052 — Every MCP server is untrusted until the owner says otherwise
Status: accepted · 2026-10-11
Every MCP server, local or remote, is outside content unless the owner trusts it.

## D-051 — Trust gates: what steers every session, project instructions, the verifier, reviews
Status: recorded as proposed; partly superseded by D-053, D-064, D-067 · 2026-10-10
Child and risky sessions can't change prompt files or active skills; no session writes the owner's config or secrets. Project AGENTS.md/CLAUDE.md steer only from trusted repositories. Unknown ratings fail closed; approvals name the reviewed fingerprint.

## D-050 — Evals run only the affected tasks, in parallel, against a cached base
Status: accepted · 2026-10-10
`scripts/eval.sh` runs the tasks a change's areas touch plus `core`, in parallel, reusing a cached base; `--plan` explains, `--full` runs all.

## D-049 — A Matrix channel: zen-matrix, a separate bridge process
Status: accepted · 2026-10-09
`zen-matrix` is a separate kernel client answering only `MATRIX_OWNER` in end-to-end encrypted rooms: one per session, plus **zen jobs**.

## D-048 — The sessions board is zen's home screen
Status: accepted; amended by D-057 · 2026-10-09
Full-screen `zen` opens on the sessions board; subagent and verifier sessions open read-only.

## D-047 — Session names and suggested next prompts from a cheap side model
Status: accepted · 2026-10-09
After each owner turn an assist model names the session (never an owner-titled one) and suggests a next prompt Tab accepts; outcomes recorded.

## D-046 — Scheduled jobs in the kernel; the sleep and engine updates move off systemd timers
Status: partly superseded by D-066, D-068, D-076 · 2026-10-09
One kernel scheduler runs system jobs and agent jobs in fresh `job` sessions, never repeating a run. An agent-created job goes live only if System One finds the owner asked for it, else it waits for the owner. Failing jobs back off and pause after 5.

## D-045 — IDENTITY.md; the agent keeps its prompt files; memory is promoted into them and the wiki
Status: partly superseded by D-069 · 2026-10-08
`SOUL.md` is owner-only; the agent writes lasting traits into `IDENTITY.md`/`USER.md` itself and says so; capped, backed up first.

## D-044 — One safe cross-provider continuation on hard usage limits
Status: accepted; amended by D-058 · 2026-10-08
On a hard subscription usage limit, `zen-engine` continues once on the other provider, visibly, never repeating an uncertain tool call.

## D-043 — Retire Pi; call System One directly from the kernel
Status: accepted · 2026-10-08
No Pi; `zend` calls OpenRouter's System One directly.

## D-042 — Keep the dev kernel home outside the live home
Status: accepted · 2026-10-08
The dev kernel's home is `~/.zenbot-dev`.

## D-041 — Loopback listener and one boundary for untrusted tool results
Status: accepted · 2026-10-08
`zend` listens on loopback by default; tokens go in headers; every tool result is secret-masked; workers don't inherit secrets.

## D-040 — zenbot's home is laid out by scope; agents are thin definitions
Status: accepted; refined by D-042 · 2026-10-07
Home layout: system files at the top, `agents/<name>/`, shared `global/`, `projects/<id>/`.

## D-039 — Full screen owns the screen: diffed frames, scrolling, a side panel
Status: accepted · 2026-10-07 · supersedes part of D-014
Full screen redraws only changed rows and can show a file beside the chat (`/open`).

## D-038 — Subagents, and model choice that learns from verdicts
Status: partly superseded by D-066, D-074 · 2026-10-07
`delegate` runs child sessions with the parent's tools but `ask`/`delegate`, in parallel.

## D-037 — The workshop: rules the kernel enforces for skills and tools the agent makes
Status: partly superseded by D-069, D-070 · 2026-10-07
Skills change only via `save_skill` (reason, format, ≤ 10,000 chars, no near-duplicate); unused ones archive at 90 days.

## D-036 — The wiki: pages over timelines; System One routes, the agent writes
Status: superseded by D-070 · 2026-10-07

## D-035 — Search: three arms in one query; promotion earns its trust
Status: partly superseded by D-045, D-069, D-076 · 2026-10-07
Search merges exact identifiers, full text and vectors (exact hits on top); tool output isn't indexed.

## D-034 — Web access: keyless search by default, untrusted content taints
Status: partly superseded by D-052, D-053, D-076 · 2026-10-07
`web_fetch` reaches only public addresses, checking every redirect; web content is enveloped as outside content.

## D-033 — MCP behind three fixed tools; a small client of our own
Status: accepted; extended by D-071 · 2026-10-07
MCP tools stay behind `find_tools`/`load_tool`/`call_tool`, via the kernel's own client.

## D-032 — System One may see private content
Status: accepted · 2026-10-07
System One may see private content, secrets masked; `ZEN_S1_PRIVATE=0` stops it.

## D-031 — Docs: one file per kind of fact
Status: accepted · 2026-10-06
Each kind of fact lives in one doc.

## D-030 — Model choice learned from real usage (replaces Phase 2b)
Status: accepted; built by D-038, D-074 · 2026-10-06
Models are chosen from real outcomes, switching only at session or subtask boundaries (except D-044).

## D-029 — Skills improve in a closed loop, without sprawl
Status: accepted; made concrete by D-037 · 2026-10-06
The agent improves its own skills: few domains, edit before create, small and composable.

## D-028 — Memory: fixed-size short-term, nightly sleep, a very high bar for long-term
Status: superseded by D-045, D-069 · 2026-10-06

## D-027 — Prompt files, system tools, and loading only what's needed
Status: partly superseded by D-040, D-045, D-066, D-070 · 2026-10-06
A fixed prompt and tool list per session; skills and MCP tools load as tool results, keeping the cache.

## D-026 — Tools and skills instead of a kernel-enforced workflow
Status: accepted · 2026-10-06 · supersedes D-020
Briefs are a skill, `verify` a tool; the kernel enforces only authority, safety and measurement.

## D-025 — zenbot reframed: a maker tool that works like a chief of staff
Status: accepted · 2026-10-06
zenbot carries the owner's jobs end to end; the owner drives and decides spend.

## D-024 — Branch, pull request, green CI
Status: accepted · 2026-10-06
Every change merges through a pull request on green CI; harness changes show the owner an eval.

## D-023 — Expand-only migrations, tested and backed up
Status: accepted · 2026-10-06
Migrations only add; the upgrade tests them on a copy and backs up first.

## D-022 — Vendor CLIs on their latest versions; Pi pinned
Status: partly superseded by D-043, D-046 · 2026-10-06
The `engines` job keeps Claude Code and Codex on the vendor's latest, rolling back an update that fails a test turn.

## D-021 — A session is one job; briefs opt-in
Status: superseded by D-026 · 2026-10-06

## D-020 — Briefed work with kernel-enforced gates
Status: superseded by D-026, D-030 · 2026-10-05

## D-019 — Context v2
Status: accepted · 2026-10-05 · supersedes part of D-012
Fixed instructions, an append-only hash-linked tape, engine sessions as caches (`docs/context.md`).

## D-018 — Live scoring with System One
Status: partly superseded by D-032, D-043, D-066 · 2026-10-02
The owner's verdict (`/done`) is the ground-truth label.

## D-017 — Evals compare harness versions; the owner decides
Status: accepted; refined by D-050 · 2026-10-02
Evals compare old and new harness; the owner decides, never an automatic gate.

## D-016 — Every turn traced
Status: accepted · 2026-10-02
Every turn records harness, engine, model, effort, tokens, cost and time.

## D-015 — Full model ids; explicit thinking level
Status: accepted · 2026-10-02
Full model ids, never aliases; always an explicit thinking level.

## D-014 — Terminal app layout; instruction files on demand; commit trailers
Status: partly superseded by D-039 · 2026-10-01
Project instruction files attach on first touch; zen commits carry `Zen-Session` trailers.

## D-013 — Robustness pass informed by Pi
Status: accepted · 2026-10-01
The kernel supervises workers and tools; upgrades run a scripted turn first.

## D-012 — Engines: zen-engine drives the official CLIs
Status: partly superseded by D-019, D-043 · 2026-10-01
`zen-engine` drives the official CLIs on subscriptions, their own tools off.

## D-011 — Interface: the `zen` terminal app
Status: accepted · 2026-10-01
`zen` is a terminal app plus script commands; no web UI.

## D-010 — SPEC v0.2: three layers, services first
Status: accepted · 2026-09-30
Three layers: Engine, Mind, Work.

## D-009 — Knowledge: our own take
Status: accepted · 2026-09-30
Knowledge is our own design, after Karpathy's LLM Wiki.

## D-008 — Channels
Status: superseded by D-011, D-049 · 2026-09-30

## D-007 — Subscriptions through supported paths
Status: accepted · 2026-09-30
Subscriptions through supported paths; a small API budget for System One and embeddings.

## D-006 — Postgres + pgvector
Status: superseded by D-076 · 2026-09-30

## D-005 — Rust kernel, TypeScript model worker
Status: superseded by D-011, D-012, D-043 · 2026-09-30

## D-004 — Host-agnostic
Status: accepted · 2026-09-30
zenbot runs on any Linux box.

## D-003 — Composable, open source first
Status: accepted · 2026-09-30
Composable modules, open source first, every part switchable.

## D-002 — Developed in public
Status: accepted · 2026-09-30
The repository is public.

## D-001 — A personal tool, not a product
Status: accepted · 2026-09-30
zenbot is a personal tool, not a product.
