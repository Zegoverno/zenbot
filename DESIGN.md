# zenbot — Design

> How zenbot works today. Why it is this way: [DECISIONS.md](DECISIONS.md). Where each piece lives:
> [MAP.md](MAP.md). Each module's header comment is its design note; read it before changing the
> module. Deep dives: `docs/context.md`, `docs/worker-protocol.md`, `docs/client-protocol.md`,
> `docs/matrix.md`, `docs/brief.md`. The earlier long-term target is history:
> `docs/archive/SPEC-target-2026-10.md`. Kernel modules below are in `crates/zend/src/`.

## Invariants

Changing one needs the owner's OK and a DECISIONS.md entry.

1. **The kernel owns all state and runs every tool call.** Workers are stateless. Engines run with
   their own tools off, and a run that shows or uses one is stopped.
2. **The tape is the source of truth.** The model reads only what the tape holds, and the tape
   holds everything the model read. Engine sessions are a disposable cache.
3. **The prefix is fixed per session.** Instructions and tools are set once; per-turn context goes
   at the end; history is only appended to, so the provider's prompt cache holds.
4. **Every turn is measured**: build, engine and version, model, effort, cache breaks, tokens,
   cost, time, tool errors.
5. **The owner's verdict (`/done`) is the ground truth.** Evals inform; they never gate.
6. **Secrets never reach the model, the tape or logs**; outside content never steers or authorizes.

## The pieces

```
 zen (terminal, scripts)      zen-matrix (Matrix rooms)
        │ HTTP + WebSocket, loopback, owner token
 zend: API · sessions/tape · context compiler · tools · gate · S1 · brain · jobs · router
        │ JSON-RPC over stdio            │ SQLite file        │ HTTPS
 zen-engine: Claude Code CLI, Codex      zen.sqlite3         OpenRouter (S1, embeddings),
 app-server, faux (tools back to zend)                       web search
```

- **Kernel `zend`** (`crates/zend`): one always-on binary, the systemd service `zenbot`.
- **Worker `zen-engine`** (`crates/zen-engine`): a child of the kernel, supervised and restarted.
  It runs each turn as one CLI process on the owner's subscription: Claude Code (built-in tools
  off, zenbot's tools through an MCP bridge, `system/init` checked) or `codex app-server` (zenbot's
  tools as dynamic tools, any foreign item stops the turn). Models are `<engine>/<model>`; the
  kernel routes each to the worker that lists it. `faux/smoke` (`ZEN_FAUX=1`) runs scripted turns
  for tests. A hard usage limit may continue once on the other subscription engine (D-044).
  Background turns share an engine cap (`ZEN_ENGINE_CONCURRENCY`, D-075).
- **Clients**: `zen` (`crates/zen`, a full-screen sessions board, `--inline`, or `--json` script
  commands) and `zen-matrix` (`crates/zen-matrix`, one encrypted room per session plus a jobs
  room, D-049). Both use `crates/zen-client`; message types are in `crates/zen-proto`. Clients
  drop invisible characters that hide or reorder text (D-077).
- **System One** (`s1/`, D-066): every fast decision is a TOML file in `crates/zend/s1/` (owner
  overrides in `~/.zenbot/s1/`): its questions, the facts it sees, a policy of costs or rules, an
  `unavailable` outcome, a ladder of approved classifiers on OpenRouter, and test cases. The
  catalog fixes in code what each caller sends, so a decision about outside content can only lead
  toward caution. Every call is recorded; the owner's answers label outcomes for calibration.
- **Storage** (`storage.rs`, D-076): one private SQLite file, `~/.zenbot/zen.sqlite3` (WAL, full
  sync, short immediate writes), with bundled sqlite-vec and FTS5. Migrations in
  `crates/zend/sqlite-migrations/` are expand-only; the PostgreSQL ones in
  `crates/zend/migrations/` are history, imported once by `scripts/migrate-storage.sh` during an
  upgrade. The tape is mirrored to private JSONL files in `~/.zenbot/sessions/`; memory, records
  and skills are Markdown files (principle 11).

## A turn

1. A client sends a prompt. A prompt that arrives while a turn runs is queued durably; a steering
   prompt stops the turn at the next tool boundary and starts a correction turn (`turns.rs`).
2. The owner's message is appended to the tape (`tape.rs`: per-session `seq`, parent link, hash
   chain, a kernel-owned risk rating per block). Only the authenticated owner path writes risk 0.
3. If nobody chose the model, the router (`route.rs`, D-074) asks S1's `task_kind` and takes that
   kind's route from the versioned policy (a small share tries a candidate). The owner's choice
   wins.
4. The compiler (`compile.rs`) sends the envelope (instructions and tools, fixed for the session)
   → a summary of older turns → the history → the prompt with its turn context. Past the soft
   limit, older turns are summarized in the background (`compact.rs`); `history` reads any block
   back. Each session keeps a matching engine session so earlier turns come from the cache.
5. The worker streams text; each tool call comes back as `tool.call`. The kernel gates it, runs
   it, masks and rates the output, and records it (`dispatch.rs`, `tool_calls`). Read-only calls
   may overlap; anything else runs alone, in order.
6. At the end the kernel records the turn (`turns`), System One keeps sentences of the owner's
   message worth remembering (`memory_capture`), the assist model names the session and suggests
   the next prompt (`assist.rs`, D-047), and the indexer adds the turn to search. A watchdog stops
   stalled turns.

## Context

The instructions are the prompt files under `~/.zenbot` (`layout.rs`): the agent's `SOUL.md` and
`IDENTITY.md`, then `AGENTS.md` (the environment) and `USER.md`, short-term memory as of the
session's start, the skills index, then `AGENTS.md` or `CLAUDE.md` from `/` down to the workspace
(`context.rs`). A project's instruction file found later by a tool is attached to that result:
as instructions in a trusted repository (zenbot's checkout, `ZEN_TRUSTED_REPOS`), otherwise as
untrusted content. The kernel writes missing defaults from `crates/zend/defaults/` and never
overwrites. Nothing else is pushed into a turn: skills, MCP tools, memory records and past sessions
are found on demand, as tool results, so the prefix never changes.

## Tools and the kernel's authority

One fixed list per session, in a fixed order (MAP.md lists the tools and which session kinds get
them; `agent.rs` enforces it): files and shell (`read`, `write`, `edit`, `bash`), recall (`search`,
`history`, `brain`, `remember`), skills (`find_skills`, `load_skill`, `save_skill`), MCP
(`find_tools`, `load_tool`, `call_tool`), web (`web_search`, `web_fetch`), work (`ask`, `verify`,
`delegate`, `schedule`, `judge`). Output past its budget shows head and tail, the full masked text
saved under `~/.zenbot/outputs` (D-071).

- **MCP** (`mcp.rs`): servers in `~/.zenbot/mcp.json`, stdio or streamable HTTP, untrusted until
  the owner says otherwise; stdio servers run contained behind their own egress proxy. Remote
  sign-in pastes back the redirect address (D-072).
- **Web** (`web.rs`): public addresses only, checked on every redirect; results wrapped as
  untrusted and rated by source; search through a native SearXNG (free, D-078), or Brave or Tavily with a key, reranked by S1.
- **Delegation** (`delegate.rs`): subagents run tasks in parallel, inherit the parent's risk, and
  can't ask, delegate or schedule. A child stops with the turn that waits for it.
- **Briefs and verification**: skills plus `verify` and `ask` (`docs/brief.md`).

## Security

- **Access.** One owner token, sent only in the `Authorization` header; the listener binds to
  loopback unless `ZEN_BIND` says otherwise. Workers and shells get no kernel secrets; secret
  values and known token formats are masked in every output (`secrets.rs`). The agent can't write
  owner files (config, secrets, the database); shells don't see secrets or the database.
- **Contained shells** (`tools.rs`, D-067). Every session's `bash` runs in bubblewrap: the machine
  read-only, the workspace writable unless the session's risk is high or it's locked, the zen home
  read-only with secrets hidden, private `/tmp` and `/run`, its own processes, no sudo, no host
  sockets. A verifier's shell is fully read-only and offline. Files written by a denied or level-5
  session are masked from every shell (`fsview.rs`, D-064). `host: true` runs as the owner, on
  the host, only past the gate.
- **Egress proxy** (`proxy.rs`). A shell's only network is a per-session socket to the kernel;
  each new host is decided by S1's `egress` from the session's risk and the host's rating, and
  recorded. A held host waits for the owner.
- **Risk levels and provenance** (`taint.rs`, `sources.rs`, `trust.rs`; D-062, D-065). Every
  block and stored item carries a 0–5 origin rating that survives copying: the owner 0, trusted
  sources 1, reputable 2, user content 3, unknown 4, blocked 5. The owner's `~/.zenbot/trust.toml`
  rates first; S1 rates the rest from metadata only, and content checks only raise. A session's
  risk is the highest it has read: 0–1 low, 2–3 medium, 4+ high. From 2 content is labelled or
  wrapped as untrusted; at 4 the session can't steer or vouch and its shell is offline.
  The owner's `/deny` or level-5 content stops a session and its dependent readers; `/trust` lifts
  reviewed causes forward only (D-054).
- **The action gate** (`gate.rs`, D-067). Before any effect, S1's `action_gate` weighs the context
  risk, the effect (local, persistent, outward, irreversible) and, when needed, whether the action
  follows the owner's job (`task.rs`: the goal from authenticated owner tape, never tool output):
  run, hold (a queue item, `queue.rs`) or lock (read-only and offline until the owner reviews).
  Owner grants are exact, single-use.
- **Boundary.** `zend` and approved host commands run as the owner's Unix user.

## Memory, knowledge and skills

Three curated stores (D-069), Markdown files as the source of truth and the database an index
rebuilt from them (`zen brain index`). Session history is a searchable archive, not memory.

- **Short-term memory** (`memory.rs`): `~/.zenbot/global/MEMORY.md`, capped (`ZEN_MEMORY_CHARS`)
  and in every session's instructions. `remember` adds, replaces or removes an entry; the owner
  may edit the file. Entries rated 4+, or with a denied writer, are listed by id only.
- **Records** (`brain/`): long-term memory, knowledge and entities (people, organizations,
  projects), one file each with JSON frontmatter: tags, aliases, subjects, evidence with its basis,
  typed links. The agent writes only through `brain` put (schema and revision checked, old version
  backed up). Legacy flat wiki pages migrate as provisional sources (D-070, `wiki.rs`).
- **The sleep** (`brain/sleep.rs`; nightly, when memory is full, or `zen memory sleep`): S1 moves
  lasting short-term memories to long-term records and archives finished ones. Nothing is deleted.
- **Search** (`search.rs`): exact names and paths, then FTS5 full text and sqlite-vec meaning,
  merged by reciprocal rank fusion and reranked by S1; it follows typed links from kept records,
  two hops at most. Results keep their ratings.
- **Skills** (`skills.rs`, `workshop.rs`): agentskills.io folders in `~/.zenbot/global/skills/`,
  indexed in the instructions and loaded as tool results. `save_skill` refuses near-duplicates;
  new skills and revisions of active ones are drafts until the owner accepts them by fingerprint.

## Jobs and routing

One scheduler in the kernel (`jobs.rs`, D-046), tables `jobs` and `job_runs`:

- **System jobs**, seeded at start: `sleep`, then `wiki`, `skills`, `routes` and `prune` upkeep,
  and `engines` (`scripts/update-engines.sh`: latest CLIs, tested, rolled back on failure).
- **Agent jobs**: a prompt in a fresh `job` session with the instructions it picks; no `ask`,
  `delegate` or `schedule`; the final answer (or a `report_file`, D-055) is the report, `[SILENT]`
  when nothing needs attention. The session is archived when the run ends. A report can be posted
  into one of the owner's sessions (D-060) or reaches the jobs room and `zen jobs runs`.
- **Script jobs** (D-073): a shell command in the contained shell, no model.
- An agent's job goes live only when S1 judges, from the owner's own messages, that the owner
  asked for it; a script job the agent writes always waits for the owner. `ZEN_JOBS=0` turns the
  scheduler off.
- **Routing** (`route.rs`, `delegate.rs`): the nightly `routes` job grades each kind of work and
  model on the owner's verdicts, cost and time, and switches a route only on clear evidence, as a
  policy version `zen policy undo` reverts.

## Upgrades

`scripts/upgrade.sh` builds or fetches CI's binaries for the commit (`scripts/fetch-release.sh`,
x86_64 and aarch64), runs the checks and a faux smoke test, then `scripts/apply-upgrade.sh` waits
until no session is working, tests the migrations on a backup, installs, health-checks
(`/health`) and rolls the binaries back if the kernel doesn't come up. It also rebuilds an
installed `zen-matrix` whose sources changed (D-077). `zen upgrade` runs
`scripts/self-update.sh` (pull, then upgrade).
