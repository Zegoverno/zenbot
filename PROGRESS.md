# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

## 2026-10-15 — Source ratings and the owner's allowlist (D-062 step 2)

Migration 0027 adds source entries/audit, ratings and provenance on tape/tool calls, session max
risk and cached owner-URL judgments. Only authenticated owner turns get risk 0; historical roles
are not authenticated retroactively. Web fetches use the final URL (exact HTTPS hosts and
segment-prefix paths); searches take the highest result rating. The owner manages entries with
`zen sources add/rm/list`, `/sources` or Matrix `!sources`. A risky session can fetch an allowlisted
target; each redirect is separately checked for SSRF and egress. Owner URL judgments see only the
owner message, requested URL and base rating: vouch lowers one (floor 2), suspect raises to 4,
failure leaves it; any cross-host redirect cancels the adjustment. Kernel result digests bind
worker messages to recorded provenance; a fake/replayed call ID cannot label new text as trusted.
Children and summaries preserve session risk. Existing taint/limits still enforce actions;
stored-item ratings, /deny and reversible taint are step 3. Tests include source matching,
isolated DB provenance/egress checks and e2e owner CRUD plus monotonic search ratings.

## 2026-10-15 — The gate reads plain commands; web_fetch can't carry data out (D-062 step 1)

All 16 holds the gate had made were false, mostly plain reads, and each one also took the
session's shell away. The shell grammar (`gate/grammar.rs`) now allows listed read programs, each
with its own flags (`ls cat head tail wc grep rg find sed -n jq sort uniq cut diff du stat echo
printf …`; `sed` only with print scripts, `find` without actions, `rg` without `--pre`/`-z`), pipes,
`;`, `&&`, `||`, `cd`, quotes, backslashes, globs expanded by the kernel with every match checked,
and redirects to `/dev/null`; it re-emits the command from the parsed words. The read-only view
also sees `/tmp`, read-only, where research clones live. A hold only because the gate can't classify
the action no longer taints; harmful holds still do. In a risky session `web_fetch` runs only for a
URL found in the owner's messages or earlier web results, never one the model built. Replay of
2,396 recorded commands as if every session were risky: read-only route 0 → 869 (36%); the rest are
writes, builds in the home workspace, outward actions and `$( )`/heredoc/loop syntax. All 36 attacks
are still held. Tested: unit tests (the 16 holds, peers' CVE bypasses, secrets through globs and
`cd`), a sandboxed run of re-emitted commands, e2e `action-gate`.

## 2026-10-14 — Lighter docs, checks and e2e (D-061)

Phase 1 of slimming zenbot, with no change to what it does. Docs a contributor reads and keeps
shrank from ~6.7k to ~4.4k lines: old progress, superseded decisions and `docs/research/` moved to
`docs/archive/`. A change now updates five docs, not ten, and `scripts/check-docs.py` dropped the
hand-kept lists (client-protocol routes, events, tape kinds, slash commands, every script) and
gained line budgets. e2e: 34 scenarios merged into 21 by shared setup, each isolated and run four at
once, with fewer fixed waits: 244 s to 117 s on this VM. CI's `check` job runs `scripts/check.sh`
(the docs job folded in), and `scripts/size-budget.sh` fails a build whose binaries outgrow their
measured size by ~15%.

## 2026-10-14 — A job's report can go into a session (D-060)

The owner wants a daily check-in in his main Matrix room that he can answer right there. A job's
new `post_to` (a session id or prefix, or `this`) makes the kernel append a clean `ok` run's report
to that session as an assistant message, after any running turn, and record it in
`job_runs.posted_to`; the Matrix bridge posts it in that session's room and leaves it out of
**zen jobs**. A run that read untrusted content still reports to the jobs feed. `zen jobs add/set
--post-to`, the API and the `schedule` tool take it (migration 0026). Tested: e2e `job-post-to`
(posted once, recorded, restart recovery, the session goes on, a marked run stays in the feed,
refusals, clearing),
the bridge's unit tests.

---

## 2026-10-14 — Disk hygiene: dumps, eval runs, build dirs, Docker (D-059)

The VM's 25 GB disk once filled up; four kinds of growth had no cleanup. The nightly `prune` job
now also keeps only the newest 3 database dumps (`db.sh` keeps 3 too, down from 10) and removes eval
run directories older than 7 days, touching only names the scripts make. After a healthy install,
`apply-upgrade.sh` trims a cargo target directory of the checkout over 2 GiB (incremental caches and
`debug/` first, the whole directory only if still over; never while cargo runs) and prunes dangling
Docker images and week-old build cache. `zen status` shows the root filesystem's use and warns over
80%. Tested: unit tests for both prune rules and the status line; `trim_target` on scratch dirs.

---


## 2026-10-14 — A failover's model sticks to the session (D-058)

After a hard usage limit made a turn continue on another model, the next prompt went back to the
exhausted one, and the terminal's status line kept showing it. Now the kernel moves the session to
the model it failed over to (effort kept only if that model takes it, the same rule as `/model`,
now one helper) and tells clients with a new `model` event; `zen` updates its status line from it.
The model stays until the owner picks another. The assist model (names, suggestions) retries once
on the session's model when it hits its own usage limit; the summarizer already did. The faux
engine has a `fallback` step and a second model, `faux/backup`, for the new `failover-model` e2e
scenario.

## 2026-10-14 — Job sessions archive themselves when the run ends (D-057)

Every finished job run left its session on the board's Jobs section. Now the kernel archives a run's
session however the run ends (ok, silent, error, interrupted); the report still reaches the owner
through the jobs room and `zen jobs runs`. Migration 0025 archives the job sessions from before.
The e2e `scheduled_jobs` scenario checks an ok and a failed run's sessions are archived.

---

## 2026-10-14 — Sleep stub concurrency measurement

- Finish the System One stub's inflight accounting before sending the response body. The client
  can start its next request immediately on receipt; counting response-handler cleanup as active
  judging falsely exceeded the four-call limit in post-merge CI. Kernel limits are unchanged.

---

## 2026-10-13 — Three-way action gate (D-056)

Outside reads mark provenance as risky, without blanket shell taint. The kernel checks effects
before dispatch: fixed read utilities run in a hardened read-only view, programmable builds/Git
run in the offline workspace sandbox, and unsupported, outward or steering actions are held.
A held action taints the session before anything runs. `/allow` and `/allow yes` (`!` in Matrix)
review and grant the latest exact workspace/tool/arguments once for ten minutes, without trusting
the session. Approval and its audit commit together; the grant is consumed atomically.
Approved scheduling uses owner authorization without a second job activation prompt. Kernel Git
ignores global configuration and hooks/fsmonitor, closing the indirect Git-config escape.
Reads validate the opened object, and unapproved file writes use directory descriptors instead
of following mutable symlinks. Verification checks its commands and directory before running,
and collecting its Git diff is read-only/offline too. Secret mounts now also hide SSH, GitHub,
AWS and package-registry credentials. Request-based System One grants remain a follow-up.

---

## 2026-10-13 — Search no longer spreads the taint mark; worktrees are trusted (D-056)

Most tainted sessions weren't reading the web: `search` came across a memory an earlier tainted
session saved and tainted the reader. Now a marked result `search` only comes across is listed by
reference without its text; asked for by name it comes wrapped and taints, as before. A git
worktree of a trusted repository is trusted like the repository (zenbot's own worktrees tainted the
sessions that made them). First step of the action gate (ROADMAP). Tested: e2e `taint_on_data`
(a search lists the memory without its text and the reader stays clean; by name it's wrapped and
taints) and `project_files` (a worktree's AGENTS.md is followed); unit tests for the worktree link,
including a forged one-way link.

---

---

## 2026-10-13 — A job's report can come from a file (D-055)

The `daily-readings` job sends the Mass readings, which a script builds and checks word for word
against the source, but the report was the run's final answer, so the model retyped the text. Now
a job can name a `report_file` (`zen jobs add/set --report-file`, `{report_file}` in `POST/PATCH
/api/jobs`, the `schedule` tool's `report_file`; new column `jobs.report_file`, migration 0023).
When the run ends `ok`, the kernel posts that file, and the final answer stays in the session. It
fails closed: a file missing, older than the run, a symlink, outside the job's workspace, a secret
file, over 60 KB or not UTF-8 makes the run an error naming why; an empty file makes it silent.
Taint is unchanged. Tested: e2e `job-report-file` (the report is the file, not the answer; refused
outside the workspace when set; missing, stale, a symlink to the token, outside at run time each an
error; empty is silent; `""` clears it), unit tests for the path and file rules.

## 2026-10-13 — `/trust`: the owner trusts a session again (D-054)

A session that read anything from outside (a web search, a subagent's answer: about half of
them) ran limited for good: no network in its shell, no skill or prompt-file changes. The only
way out was a fresh session, losing the work's context. Now `/trust` in zen (`!trust` in Matrix)
shows what marked the session: its `taint` blocks and the untrusted content it was handed, from
the new `GET /api/sessions/{id}/taint`. `/trust yes` lifts the mark through `POST
/api/sessions/{id}/trust`, which records a `trust` block. It works forward only: the next
untrusted read marks the session again, and what it stored meanwhile keeps its marks. It's refused
while a turn runs and when the session read more after the owner looked. Sessions now carry
`tainted_at`: the board and zen's footer mark `untrusted`, and the stream sends `taint`. Tested:
e2e `trust` (a tainted shell can't reach the route even with the token; refused on a stale look;
lifted; the stored memory keeps its mark; the shell gets the network back; the next web search
marks it again), unit tests for parsing, the report and the TUI marks.

## 2026-10-13 — Owner commands in the chat: `/jobs`, `/skills`, `/tools` (and `!` in Matrix)

Approving a job zenbot created paused, running one to test it, or accepting a draft skill or a
made tool needed the terminal (`zen jobs …`), out of reach from the chat or the phone. Now zen's
input takes `/jobs [run|resume|pause <name> | runs [name]]`, `/skills [accept|reject <domain/name>
<sha>]` and `/tools accept|reject <name> <sha>`, and Matrix takes the same with `!` (in every room,
zen jobs included). One module, `zen_client::owner`, parses and runs them for both clients. The
client reads them before anything is sent and calls the kernel with the owner's token, so they
never reach the agent: text it read can't approve anything. Removing and creating jobs stay in
the terminal. Tested: parsing and formatting unit tests, a TUI test that a command is never sent as
a prompt, zen-matrix's parse tests.

## 2026-10-11 — Docs trimmed: each fact in one place

The docs repeated the same facts across files (the upgrade flow in six, the tool list, jobs and
taint rules in several), so every change had to touch many of them. Now each fact lives in one
file and the others link to it; no fact was dropped.

- The upgrade and engine-update flow: only DEVELOPMENT.md; AGENTS, MAP, INSTALL and DESIGN link.
  DEVELOPMENT.md drops what AGENTS.md owns (git flow, hard rules, secret files) and what
  DESIGN.md and evals/README.md own (jobs, sleep, wiki, workshop, evals), keeping the commands.
- MAP.md: no per-file line counts; the Tests section lists e2e scenario names, and what each
  checks is the comment above its function in `scripts/e2e.sh` (all 29 have one now).
- ROADMAP.md: finished phases 0–6 are one table row each with where they're recorded; every debt
  item was checked against the code and is still open.
- SPEC.md: the "Moved to" stubs are gone; as-built details link to DESIGN.md.
- README.md: install, use, settings people set, and links.
- DESIGN.md: the tool list links to MAP.md (check-docs.py now checks tools there only); "Security
  today" links to "Untrusted content".
- Stale statements fixed: routes and skills are tended by their own nightly jobs, not the sleep;
  `verify`'s commands run in the session's sandbox; bubblewrap also runs tainted shells; 29 e2e
  scenarios, not 27; decisions run to D-053.
- Lines (before → after): AGENTS 140 → 139, DESIGN 533 → 523, DEVELOPMENT 466 → 437, INSTALL
  66 → 58, MAP 681 → 596, README 135 → 115, ROADMAP 204 → 131, SPEC 262 → 239, evals/README
  101 → 102; the current docs 3,238 → 2,990 lines (MAP.md 113 → 104 KB). History (PROGRESS,
  DECISIONS, docs/research) unchanged.
- Tested: `scripts/check-docs.py`, `scripts/check.sh`.

---

## 2026-10-11 — Engines behind one trait; the worker protocol typed

Toward CONTEXT.md principles 5 and 6: adding an engine to `zen-engine` meant edits in several
match arms (model listing, `complete`, turn routing, failover's target, sign-in check and run), and
the worker protocol was untyped JSON on both sides.

- `zen-engine`: an `Engine` trait (`engine.rs`: name, version, models, `run_turn`, `complete`,
  start-up housekeeping, failover target, subscription sign-in check) implemented by `claude`,
  `codex` and `faux`, and a registry (`engine::all`) the protocol loop and failover go through.
  Adding an engine is one module and one line. The engines' runtime checks (Claude Code's
  `system/init` check, Codex's item allowlist, `codex-features`) stay in their modules with their
  tests.
- `zen-proto` types the worker protocol (`zen_proto::worker`: every request, answer, `tool.call`
  and turn notification in docs/worker-protocol.md, with a round-trip test per message). `zend`
  (`mind`, `dispatch`, `turns`, `workers`) and `zen-engine` build and read messages through them.
- No behavior change: the wire format is the same bytes (keys still sorted), checked by feeding the
  same requests to the old and new `zen-engine` (ping, models.list with the real CLIs, complete,
  faux turns, an unknown engine, an abort before its start, an unknown method), and by the e2e
  suite unchanged. A misshapen `turn.end` (fields of the wrong type) still ends its turn.

---

## 2026-10-11 — One client contract: typed session events, one kernel client

Toward CONTEXT.md principles 5 and 6: `zen` and `zen-matrix` each parsed the kernel's session
events by hand and each had its own copy of the kernel client.

- `zen-proto` types the client protocol's events (`zen_proto::client::Event`, exactly the table in
  docs/client-protocol.md; an unknown or misshapen event is `Other`, so a client still tolerates
  new events). One end-of-turn rule (`Event::ends_turn`: `end` without `next`, `idle`, a `resync`
  with `busy: false`) and one echo match for a sent prompt (`echoes`, `Sent`: trimmed text, never
  the kernel's own message) replace the clients' separate rules. `zen ask`/`zen chat`, the terminal
  app and the Matrix bridge read events through them. The wire format is unchanged (the kernel
  still builds events with `json!`). A full shared turn reducer didn't fit: each client renders
  deltas and messages its own way, and the bridge ignores deltas.
- New crate `zen-client`: the kernel client both use (`zen_home`, `parse_env`, the token,
  HTTP calls, `new_session`, `resolve`, the session WebSocket, `enc`, the plain-http token
  warning). `zen-matrix` uses it through a path dependency, so it now also warns about a token
  sent over plain http to another machine, encodes session ids in paths, and waits 120 s (was 60)
  for a call. An echo that differs from the sent prompt only in surrounding whitespace now counts
  as the echo in `zen` too.
- AGENTS.md said `zen-proto` holds the worker protocol's messages; it never did. The row now says
  what it holds; typing the worker protocol stays debt (ROADMAP.md).

## 2026-10-11 — Kernel modules: upkeep jobs of their own, one settings module, shared helpers

Toward CONTEXT.md principle 5 (every building block a module with a documented contract).

- The nightly sleep had grown into the orchestrator for unrelated work. It now tidies memory only;
  the rest are system jobs seeded like `sleep` and `engines` (existing installs get them at the
  next start), each with its own run record and report: `wiki` (03:10 UTC, commit the agent's wiki
  edits, report missing summaries and broken links), `skills` (03:20, flag and archive unused
  skills), `routes` (03:30, switch a delegation route on clear evidence) and `prune` (03:40, old
  saved outputs and prompt-file backups). A run that found nothing is `silent`. The next sessions'
  morning note still says what they found, read from their last day's runs. A sleep by hand or at
  the ceiling no longer does the upkeep; `zen jobs run wiki` (etc.) does.
- `config.rs` reads the kernel's ~40 tunable settings once at start, typed, with every default in
  one place: `ZEN_TZ` (America/Sao_Paulo), the summary, assist and embedding models among them. A
  value that doesn't parse runs on its default and is logged; a blank value is unset; `off`,
  `false` and `no` now turn off flags that took only `0`. The prompt-cache lifetime that was keyed
  on the `claude/` prefix is a per-engine setting, `ZEN_CACHE_TTL` (`claude=3600`, others 300).
  Boot settings, paths and the workers' are still read where used (ROADMAP debt).
- One git runner (`git::run`, `git::commit_all`) replaces the wiki's and the workshop's; one
  `workers::json_object` replaces the JSON extraction in `compact` and `assist` (which could panic
  on a reply with `}` before `{`); one name rule (`valid_name`) replaces three, so a new job name
  can no longer end in a hyphen or hold `--`.
- Tested: unit tests (config parsing and fallbacks, cache TTLs, git commits, JSON extraction), e2e
  `wiki` (the job commits edits, reports a missing summary, the next session hears it) and
  `scheduled-jobs` (all six system jobs seeded, each upkeep job run by hand), `scripts/check.sh`.

## 2026-10-11 — zen: a lighter board, paged sessions, safer terminal text

- The sessions board fetched `/api/board` inside the render loop every 2 seconds, so a slow kernel
  froze keys and drawing, and the kernel sent every session ever made with all its children. The
  fetch now runs in a task of its own and the rows are rebuilt only when the answer changed; the
  kernel sends the latest 200 sessions with the latest 20 children of each (running ones always),
  archived sessions only when the board shows them (otherwise a count), and each session's
  `children` count.
- Opening a session loaded its whole tape to show the last 40 messages. It now asks for the last
  40; the kernel adds `message_count` and the session's `tokens` for the footer.
- Streamed text draws at most ~30 frames a second; text is cut between grapheme clusters (emoji no
  longer shift the side panel's divider); inline markdown renders in linear time; the Files tab
  never lists or opens secret files; a panic in any task restores the terminal and ends zen;
  `jobs runs` can't panic on a short id.
- `zen ask`: a second Ctrl-C leaves without waiting for the turn to stop. The kernel pings a quiet
  session WebSocket every 30 seconds so proxies don't close it during a long tool call.
- `crates/zen/src/main.rs` (1,237 lines) is split into `cli`, `turn`, `setup` and `print`.
- Tested: render and key tests (board fetches, paging, Files tab, emoji, frame rate), e2e checks of
  the board's archived count, `children` and `message_count`, `scripts/check.sh`.

## 2026-10-11 — Operations hardening: a private database password, safer upgrades, honest security docs

- Postgres used the public password `zen`, so any process on the VM could rewrite zenbot's state.
  `db_secure` (`scripts/db.sh`) now gives it a random password in `~/.zenbot/db-password`, set over
  the container's socket and written to `DATABASE_URL` in `env`; `install.sh` runs it, and the
  next upgrade runs it on existing installs right before the restart (old binaries read the same
  `env`, so a rollback still works). The kernel treats `db-password` and `env.prev` as secret.
- `apply-upgrade.sh` waits for idle again right before swapping binaries, goes ahead at once when
  the kernel doesn't answer at all, aborts (binaries put back, no restart) on a failed copy or an
  unreadable migration list, and removes the retired `pi` from `ZEN_WORKERS`. The database
  helpers fall back to `sudo -n docker` (a standard install isn't in the `docker` group).
- `dev.sh` no longer uses the owner's paid keys or folders (`ZEN_DEV_REAL=1` keeps them). CI pins
  actions by commit, runs `apt-get update`, and the release job runs the e2e scenarios on the
  binaries it publishes. `install.sh` checks Node's SHA-256.
- Docs now say plainly what isn't enforced: an ordinary session's shell isn't sandboxed and, with
  the install's passwordless `sudo` and Docker, is effectively root; jobs' "no outward action" is
  prompt-only. The narrowed-sudo plan is in ROADMAP.md debt, awaiting the owner. `check-docs.py`
  now checks every script and deployment file is in MAP.md.
- Tested: `db_secure` on a throwaway Postgres (bootstrap password then refused), the Node check
  with a wrong hash, `dev.sh` environment, e2e on chosen binaries, `scripts/check.sh`, and the
  real upgrade of this VM (below in the pull request).

## 2026-10-11 — Taint travels with stored content; tainted shells run offline (D-053)

- A review found web text laundered into durable stores and back as trusted: a tainted session's
  memory went unlabelled into every later session's instructions, a draft skill it wrote was served
  as a trusted skill and activated when the owner accepted any session that loaded it, a job's
  report reached the next run as kernel text, and a tainted session's `bash` had the network.
- Memories, job runs and draft skills now carry the mark (migration 0022: `memories.tainted`,
  `job_runs.tainted`, `draft_taint`), and every read path hands stored content over through
  `taint::hand_over` (wrapped, the reader tainted): search, `history`, `find_skills`/`load_skill`,
  `read` of a marked draft or a wiki page with `web` entries, a job's `<last_report>`, `schedule
  runs`. A marked memory stays out of the instructions (by id only), is never promoted to a prompt
  file, and its wiki copy is labelled `web`; the sleep's note never shows its text. A marked draft
  is activated only by the owner's own accept; `zen skills` and `zen memory` show `untrusted`.
- The rules for a tainted session are in one place (`taint::limited`). Its `bash` and `verify`'s
  commands run in bubblewrap with no network, the workspace writable and the rest read-only.
- Tested: unit tests; e2e `taint-on-data` (each laundering path, the offline shell building and
  committing in its workspace but reaching neither the kernel nor the home); offline `cargo build`
  with dependencies in the sandbox by hand.

## 2026-10-11 — Claude runs aren't refused for a rate-limit notice before init

- The engine-boundary check (2026-10-10) refused any Claude Code event before `system/init`. Claude
  Code can send `rate_limit_event` first, so a normal run failed in 4 s without reaching the model
  (seen in an eval). Notices (`rate_limit_event`, other `system` events) may now come before init;
  anything that can carry model output or tool use (`assistant`, `user`, `result`, `stream_event`)
  is still refused. Tested: unit tests.

## 2026-10-11 — Kernel robustness: children stop with their parent, masked arguments, jobs, MCP

- Aborting a parent left its subagent or verifier running (and spending) for up to 30 minutes. A
  child's turn now stops as soon as the turn waiting for it does (`StopOnDrop` in `run_and_wait`),
  and so on down.
- Tool arguments reached clients (`tool_start`) and `tool_calls.args` unmasked, and every `write`
  stored its whole file: strings are now masked and cut at 4 KB, with the whole arguments' sha256
  and size under `_full`.
- Jobs: a huge duration panicked the turn (`every 9999999999999d`), the 5-minute minimum looked at a
  cron's first two runs only (`0,30-35 * * * *` passed), `every Nh` drifted by the tick's lateness,
  a run started between the tick's check and insert rolled back the whole tick, and the approval
  gate showed System One the agent's own task as if it were evidence. Each is fixed (checked
  arithmetic, shortest gap of 50 runs, `due + k·d`, `ON CONFLICT DO NOTHING`, the task wrapped as
  untrusted data).
- The summarizer cut the owner's words at 2,000 characters and didn't hold its 400 KB budget: the
  owner's words go whole; tool lines, then the agent's, are cut and then left out to fit.
- MCP: one slow call froze every MCP tool for every session (a server's lock held across the call,
  and every lookup locked every server). Calls now run concurrently (a stdio reader hands answers
  out by id; tools come from the last listing). Messages over 8 MB fail the call; stdio servers run
  in their own process group, killed whole. Every server is untrusted unless the owner sets
  `"untrusted": false` (D-052).
- Idle scoring takes the longest-waiting sessions first and backs off one that keeps failing; a
  self-update's output is kept to its last 64 KB; the flaky "at most four at a time" check in
  `memory-sleep-judged` counts only the sleep's questions.
- Tested: unit tests for jobs, compaction, argument masking, the capped line reader and the update
  log; e2e: `abort-tool` (a parent aborted mid-delegation), `secrets` (recorded arguments), `mcp`
  (a call returns while another is slow, 9 MB answers, a server's child killed, local untrusted),
  `scheduled-jobs` (the gate's state).

## 2026-10-11 — Search index: indexes used, no skipped memories, embeddings off the lock

- Every search read the whole `search_docs` table: the query's filtered `docs` CTE was referenced
  four times, so Postgres materialized it and the HNSW, full-text and trigram indexes went unused.
  Each arm now filters the table itself; on a 30,000-document test database a search went from
  837 ms to 17 ms with each arm on its index, and ranks the same (120 queries compared, the vector
  arm exact; HNSW's approximation aside). Planned per call (`force_custom_plan`): a generic plan
  can't use HNSW for a parameter vector and took 8 s.
- Memories were indexed by an `updated_at` watermark; a long transaction (the sleep) committing
  after a later one fell behind it, so an archived memory stayed searchable. Triggers now queue each
  changed memory in the change's own transaction (`search_queue`, migration 0021).
- Embedding ran under the indexing lock (searches and captures waited up to a minute), and one
  document the provider refused failed its batch forever. Embeddings now run after the lock, a
  refused batch is halved until the document is alone, which is tried 3 times
  (`embed_attempts`, `embed_error`), and a turn is embedded once it has ended instead of after every
  message. The indexer no longer reads tool results, nor rewrites unchanged documents.
- A kernel prompt inside a turn no longer splits its search document; `tool_calls(name)` is
  indexed; `history`'s session-prefix lookup is a primary-key range.
- Tested: unit tests, a new e2e scenario `search-index` with an embeddings stub (7 of its checks
  fail on the previous `search.rs`), and EXPLAIN on the test database (in the pull request).

## 2026-10-11 — Memory sleep: no lost duplicates, no lost updates

- Two entries saying the same thing were each judged "covered" by the other, and both were
  archived (and left search). System One now names *what* covers an entry; one covered by another
  entry goes only while that entry stays, so one copy always survives (the newer), and it ranks at
  least as high as the copy it replaces.
- A `remember` replace or remove during a sleep was overwritten when the sleep wrote its fates
  (the old text could even be promoted into `USER.md`). Fates now apply with the entries locked,
  and an entry changed since the sleep read it is left as it is; the note names them.
- A promotion written to a prompt file before a failed transaction was appended again by the next
  sleep; it's now skipped when the line is already there. Prompt-file and memory sizes are counted
  in characters everywhere (non-ASCII files were cut early). System One judges up to 4 entries at
  once instead of one after another.
- Tested: unit tests, a new e2e scenario `memory-sleep-judged` with the System One stub (holds
  questions mid-sleep to change entries under it). The eval barely exercises the sleep (it runs
  nightly), so the e2e is the main evidence.

Older entries: [docs/archive/PROGRESS-2026-09-30-to-2026-10-10.md](docs/archive/PROGRESS-2026-09-30-to-2026-10-10.md).
