# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

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
## 2026-10-10 — Matrix channel: trust, rooms, no lost answers, a confined service

- A review of `zen-matrix` found it trusted the sender's user ID alone, joined any room the owner
  invited it to, lost answers on a reconnect or restart, and cut long answers by characters. Fixed
  together (docs/matrix.md, Security):
- Trust: matrix-sdk decrypts only what cross-signed devices sent (`TrustRequirement::CrossSigned`),
  and the bridge checks again: plaintext messages and the owner's unverified sessions are refused
  and logged. The owner's cross-signing master key is pinned on the first accepted message; a
  changed one is refused until `zen-matrix repin-owner` (service stopped) clears the pin.
  `MATRIX_OWNER` equal to `MATRIX_USER` is refused; the bot never acts on its own messages.
- Rooms: invites are joined only from the owner, to rooms not known to be unencrypted, then checked
  in full; before each post and on every membership change the bot leaves a room that isn't
  encrypted or holds anyone but the owner and itself. The rules are pure functions in the new
  `src/trust.rs`, unit-tested.
- No lost answers: `state.json` keeps each room's last session `seq`; on each connection, `resync`
  and new message the bridge fetches what came after (`GET /api/sessions/{id}?after=`) and posts
  it. Every mapped room is linked at start.
- Long answers are split in bytes (about 20 KB per message, code fences closed and reopened, at most
  8 parts); a failed send posts a short notice. Raw HTML in model output is shown as text in the
  formatted body. Ids from `state.json` are validated on load; no lock is held across a kernel call.
- `deploy/zen-matrix.service` is confined further: a tmpfs home with only the files it needs bound
  in, no devices or capabilities, IP and Unix sockets only, `@system-service` system calls.
- docs/client-protocol.md said live `message` events carry `seq`; they don't (the code wins).
- `scripts/matrix-e2e.sh` also checks the refusals, the restart catch-up, the room rules and the
  self-owner check.

## 2026-10-10 — Engine boundary: run-time tool checks, aborts, the tape and usage

- A review of the worker boundary found engines trusted to keep their own tools off, and gaps where
  the tape, aborts or usage could go wrong. Fixed together:
- Codex: a turn (or completion) in which Codex starts anything but a message, reasoning, a plan, a
  compaction or one of zenbot's dynamic tools is stopped and fails naming it; turning features off
  is no longer the only guard. More tool features are off, and `zen-engine codex-features` fails on
  any feature on by default that zenbot hasn't reviewed: the daily `engines` job runs it after an
  update (rolling back) and on its own (failing the job).
- Claude Code: a run must start with `system/init` and its tool list, or it is refused; `complete`
  now reads the stream too, so summaries get the same check. It is killed after 9 minutes, as is
  Codex's.
- Aborts: the kernel names the turn in `turn.abort` and sends it again if the turn was aborted while
  `turn.start` was on its way; zen-engine handles both in arrival order and remembers turns aborted
  before they started.
- The tape: tool results Claude Code gave the model itself go on the tape, and a turn that ends
  with a tool call unanswered records it as interrupted (new e2e scenario `abort-tool`). A tool
  call that never had the model's announcement fails the turn instead of getting an invented id
  silently.
- Usage: a failover turn's totals add up each provider's report instead of only the model calls
  (Codex reports tokens only per turn); a Claude turn that fails early still reports.
- Codex's version and model list are asked again when the binary changes (the list also hourly);
  listing models uses zen's private Codex folder, is limited to 15 s and holds no lock while it
  waits, and a worker whose `models.list` fails keeps its models in routing.
- Workers get none of the kernel's secret-named variables (not just five of them), the engines'
  sign-ins excepted, and start with `bash -c`, not a login shell. A kernel tool call that panics
  still answers and no longer leaves the watchdog off for its turn.
- A name in `ZEN_WORKERS` without its `ZEN_WORKER_<NAME>_CMD` (the retired `pi`, which rolled back an
  upgrade on the dev VM) is ignored with a warning, shown in `/health` and `zen status`, instead of
  failing the health check (new e2e scenario `engine-boundary`).

## 2026-10-10 — Trust gates: protected files, trusted repositories, the verifier's sandbox

- A review found ways for text from outside (a web page, an MCP server, a cloned repository) to
  reach what steers every session or the kernel's credentials through the kernel's own tools. Fixed
  together (D-051, proposed):
- `edit`/`write` protect active skills and made tools like the prompt files (refused to tainted,
  child and kernel sessions; drafts stay free), and the owner's config and secrets (`mcp.json`,
  `env`, `token`, `matrix.env`, `matrix/`, `bin/`) from every session. Paths through folders that
  don't exist yet, `..` and symlinks are resolved. `mcp.json` fills a secret-named `${VAR}` only for
  a server that lists it in `allow_env`.
- A project's AGENTS.md or CLAUDE.md found by a tool is followed only from zenbot's checkout or a
  repository in the new `ZEN_TRUSTED_REPOS`; any other comes as untrusted content and taints the
  session (new e2e scenario `project-files`).
- The verifier's shell has its own processes (the kernel's `/proc/<pid>/environ` held the token and
  database URL), no network and an empty home; its `read` stays inside the work it checks.
- Taint fails closed: an unreadable taint counts as tainted, and content whose taint can't be
  recorded is withheld. A session kind the kernel doesn't know gets no tools. `verify` refuses
  duplicate criterion ids (a second one hid the first one's failed check).
- `zen skills` shows each draft's and tool's fingerprint, and `zen skills|tools accept|reject`
  must name it; a change since is refused. A made tool runs from a private copy of its folder: what
  is checked is what runs, and its own writes (a `__pycache__`) no longer cost it its approval.

## 2026-10-10 — Faster evals: affected tasks only, in parallel, base from cache; scripts/check.sh

- Shipping took about four times as long as building, mostly the eval: every task on both harnesses,
  one kernel at a time, the unchanged base rerun on every pull request (D-050).
- `scripts/eval.sh` now runs only the tasks the change affects: changed paths map to areas in
  `evals/areas.txt`, every task names its `areas`, and three cheap `core` tasks always run. No
  harness change: it says no eval is needed and stops. `--plan` shows the selection and why;
  `--full` runs everything. A change to `memory.rs` runs 4 tasks, to `compile.rs` 9, to the
  default prompt files 6, to `zen-engine` all 22.
- Runs go 3 at a time (`--jobs`), each with its own kernel, port and database, base and new
  interleaved; Ctrl-C stops every kernel and drops the databases. With `faux/smoke`, 16 runs took
  40 s one at a time and 22 s three at a time; real turns wait on the model, so they gain more.
- Completed base runs are cached for 7 days by base commit, model, effort, task files and engine
  versions, so a second eval against the same installed base runs only the new side; the report
  lists the reused results and their dates (`--fresh` reruns them).
- `scripts/check.sh` runs what CI checks in one command, timed per step, stopping at the first
  failure.
- `archive-old-logs`' fixture logs were never committed (the task couldn't pass elsewhere); now they
  are. The coding-zen check helper moved to `evals/lib/`. MAP.md had 14 tasks (there are 23) and
  didn't list `eval-native.sh` or `eval-paired.py`.

## 2026-10-10 — Coding benchmark against the native CLIs; workspace note for Claude

- `scripts/eval.sh --native claude|codex` runs the base side through the vendor's own CLI with its
  own tools (`scripts/eval-native.sh`); `scripts/eval-paired.py` gives per-task wins and losses, a
  sign test, and cost and time ratios with intervals. Nine `coding-*` tasks: six Aider-polyglot
  exercises with hidden tests and three real zenbot bugs with their fixes reverted, each checked to
  fail on the starting files and pass on the reference fix.
- Pilot, Opus, 9 tasks: zenbot and native Claude Code each passed 8 (both fail the same Forth tests);
  zenbot cost 0.71× and took 0.74× the time. The tasks are too easy to separate the two on passing.
- Claude Code runs from a fixed engine folder and its environment block names that folder, so the
  model prefixed commands with `cd <workspace>`. The system prompt now says commands start in the
  workspace and the block describes the engine's folder; on 4 tasks the `cd`s went from 4 to 0.

## 2026-10-10 — Upgrades install the build that was tested, and log its commit

- `apply-upgrade.sh` read both the binaries (`target/release`) and the commit (`git rev-parse
  HEAD`) from the checkout when it installed, up to 30 minutes after `upgrade.sh` tested them. Two
  requests queued from two worktrees sharing one `target/` then installed together: the binaries
  were the last ones built there, and the log and `~/.zenbot/version` named whatever the other
  checkout had checked out ("now running ee84753", a commit that wasn't in the build).
- `upgrade.sh` now stages the tested binaries and their commit in `~/.zenbot/upgrades/<id>/`, and
  `apply-upgrade.sh` installs from that stage and logs its commit (with ", uncommitted changes"
  when the tree had them). Installs take a lock, and the newest request wins: an older one still
  waiting, or one after a newer install, logs `skipped`. Tested against a scratch home with stubbed
  `sudo` and health: a superseded request, a newer one installed before an older, two at once, a
  later one after those, and a missing stage.

## 2026-10-10 — The board flags sessions waiting on the owner

- With several sessions working in parallel, a question asked with `ask` was easy to miss: the
  session just looked idle. `GET /api/board` now gives each session `waiting`: its latest
  `questions` came after the owner's latest message (a message the kernel sent itself,
  `kernel: true`, doesn't count as an answer). The query reads the `(session_id, kind, seq)`
  index; 12 ms over the 86 sessions on the owner's VM.
- The board marks such a session `?` with "waiting on you", lists it first in its section, and
  counts them in the title ("? 2 waiting on you"; archived sessions aside). It clears with the
  owner's next message in that session. Tested by a render test and an e2e check in `delegation`
  (asked with `wait: false` → waiting, a session that asked nothing → not, the answer → cleared).
## 2026-10-10 — Cleanup: no web UI, no Pi leftovers, the default AGENTS.md stays out of sessions

- The frozen web UI is gone (`crates/zend/web/index.html`, `GET /`). With it goes the only use of
  a token in the URL: the kernel now accepts the token only in the `Authorization` header, the
  WebSocket upgrade included (`zen` and `zen-matrix` already send it there).
- The retired Pi worker's last traces are gone: `ZEN_WORKERS` no longer filters out `pi`,
  `install.sh` and `update-engines.sh` no longer clean it up, `zen status` no longer hides it, and
  `~/.zenbot/auth.json` (Pi's sign-in) is no longer a known secret file. The installed `env` had
  `ZEN_WORKERS=engine,pi` and was cleaned first. The two e2e checks of the legacy setting are gone.
- The default environment file is `crates/zend/defaults/AGENTS.default.md` (still installed as
  `~/.zenbot/AGENTS.md`): as `AGENTS.md`, a session working in the repo loaded it as the project's
  own instructions.
- The owner waived the eval for these. Build, unit tests, clippy and the 23 e2e scenarios pass.

## 2026-10-10 — The docs match the code again, and CI keeps them so

- Every doc was read in full and checked, claim by claim, against the code at `6217f27`. That fixed
  drift across 25 files: ROADMAP's statuses and "where things stand" (Phases 2 to 6 shown as
  unmerged), MAP's missing modules, routes, settings and tables, `zen-matrix` and `zen-proto` absent
  from AGENTS.md, protocol events and routes never documented, retired names (Pi, `/brief`,
  `ZEN_BRIEFS`, the flat home layout), missing `Superseded` markers in DECISIONS.md, and default
  prompt files that still said only the owner edits `USER.md` (D-045).
- `scripts/check-docs.py`, CI job `docs`: fails when a route, tool, setting, table, event, tape
  kind, slash command, e2e scenario or crate in the code is missing from the doc that lists it,
  when a current doc names a setting, repo path or decision that doesn't exist, and, on a pull
  request, when a change to the code adds no PROGRESS.md entry. Tested by breaking a route, a
  setting, a path and a decision number by hand: each was reported.
- AGENTS.md: "Keeping the docs true" (the code wins, present tense means shipped, history stays
  history), the check in the change loop, and a reread of the docs against the final diff before
  merging; the docs table now covers INSTALL.md, AGENTS.md, the agent's defaults and doc comments.

## 2026-10-09 — The Matrix channel (D-049)

- `zen-matrix` (`crates/zen-matrix`, its own Cargo workspace; docs/matrix.md): the owner talks to
  zen from any Matrix client. Each room is a session: a main "zen" room, `!new [title]` for more, or
  any room the owner invites the bot to. Answers come as Markdown, zen shows as typing while it
  works, `ask` questions come numbered and `1 2` answers them, `!stop` aborts. Finished job runs are
  posted to a "zen jobs" room.
- Only the owner's Matrix ID is answered; other invites are declined. Every room is end-to-end
  encrypted; `zen-matrix login` sets up cross-signing and key backup (recovery key saved, mode 600).
- `scripts/matrix.sh` builds, signs in and installs the `zen-matrix` service (confined:
  `ProtectSystem=strict`, writes only `~/.zenbot/matrix`). CI builds, tests and lints it in a
  separate `matrix` job. The kernel is unchanged.
- Tests: 12 unit tests (commands, events to posts, numbered answers, job reports, state);
  `scripts/matrix-e2e.sh` runs a throwaway homeserver (continuwuity in Docker), a faux-model
  kernel, the bridge and a scripted owner client through end-to-end encryption.

## 2026-10-09 — The sessions board (D-048)

- `zen` (full screen) opens on a board of every session: Main, Jobs and Archived sections, running
  first. Each row shows running or idle, the model and the time since the last activity, with
  subagents and verifiers nested under their parent with their task. Enter dives in, `n` starts a
  new session, `/` filters, `a` shows archived sessions, `q` quits; `/board`, or Esc on an empty
  input, comes back without stopping a running turn.
- Subagent and verifier sessions open read-only: no sending, and Esc and Ctrl+C don't interrupt
  them.
- Kernel: `GET /api/board` (every session with `kind`, `parent`, `busy` and the child's `task`, no
  costs); `kind` and `parent` added to every session's JSON.
- Tests: 7 render/key tests in `tui/board.rs`; the `delegation` e2e scenario checks a running
  subagent, with its task, under its running parent, and both idle after.

## 2026-10-09 — Session names and suggested next prompts (D-047)

- After each owner's turn, one call to the assist model (`assist.rs`; `ZEN_ASSIST_MODEL`, Haiku 5.5)
  names the session (while the title isn't the owner's, first 3 turns; `title` event) and suggests
  the next prompt (`suggestion` event; not after `ask`, nor in jobs or child sessions;
  `ZEN_SUGGEST=off`).
- zen shows the suggestion in grey in the empty input with "tab: use suggestion": Tab takes it,
  Enter sends, or edit first; typing replaces it. The side panel's focus key moves to Shift+Tab.
- The next prompt reports the suggestion; `prompt_suggestions` (migration 0020) records
  accepted / edited / declined / unseen with what was sent, the prompt version, latency and cost.
  `zen suggestions` and `GET /api/suggestions` show the rates. `sessions.title_source` keeps an
  owner's title from being renamed.
- Tests: 3 kernel unit tests (reply parsing, outcomes, what the model reads), 5 key/render tests in
  zen; e2e scenario `names-suggestions` (12 checks) with `scripts/e2e/ws_prompt.py`, a stdlib
  WebSocket client.

## 2026-10-09 — Scheduled jobs in the kernel; the sleep and engine updates leave systemd (D-046)

- A scheduler in `zend` (`jobs.rs`, tables `jobs` and `job_runs`, migration 0019) runs the
  kernel's own jobs (`sleep` 03:00 UTC, `engines` 04:00 UTC, seeded at start) and agent jobs: a
  prompt run in a fresh session of kind `job` with only the instructions it picks (soul always;
  default user and memory) and its named skills, no `ask`/`delegate`/`schedule`, ending with a
  report (`[SILENT]` runs are archived, `[FAILED]` counts as a failure).
- The owner manages jobs with `zen jobs` (`add`, `set`, `pause`, `resume`, `rm`, `run`, `runs`)
  and `/api/jobs`; the agent with the new `schedule` tool. An agent's job goes live only if System
  One, reading the owner's own messages, gives ≥ 0.9 (`ZEN_JOB_BAR`) that the owner asked for it;
  otherwise (or in a tainted session) it's saved paused, and `zen jobs resume` approves it.
- Claiming with `FOR UPDATE SKIP LOCKED` and the next run set in the same transaction; one running
  run per job (partial unique index); runs cut by a restart marked `interrupted`; a missed run
  caught up once within its grace, else recorded `missed`; failing agent jobs backed off and paused
  after 5 failures. Patterns from Hermes and OpenClaw, read in code. pg_cron considered and
  rejected (D-046).
- The `zen-sleep` and `zen-engines` timers and `scripts/sleep.sh` are gone; `install.sh` and the
  first healthy upgrade remove the installed units. Smoke, e2e, eval and dev kernels run with
  `ZEN_JOBS=0`. `zen status` shows a `jobs` line. New deps: `croner`, `chrono-tz`.
- Tests: 5 unit tests (schedules in a timezone, `every`/`at`/`in`, the 5-minute floor, grace,
  context); e2e scenario `scheduled-jobs` (22 checks); the full e2e suite passes.

## 2026-10-08 — IDENTITY.md; traits and guidance leave memory (D-045)

- New prompt file `agents/zenbot/IDENTITY.md`, loaded right after `SOUL.md`: the agent's character
  and how it works. `SOUL.md` is now the deep layer only (what the agent is for, what it holds to,
  the lines it doesn't cross); the default files are split the same way.
- Traits, guidance and preferences are not memory: they go to `IDENTITY.md` or `USER.md` (see
  below). The `remember` and `capture` descriptions say so.
- No more long-term tier (there were no long-term rows). The sleep promotes on its own instead: a
  lasting entry about the owner moves to `USER.md`, lasting guidance to `IDENTITY.md` (under
  `## Learned`, dated backup in `~/.zenbot/backups/prompt-files/`, only the owner's words or verified results,
  lowest of three samples ≥ `ZEN_PROMOTE_BAR` 0.9), and lasting knowledge is copied into the wiki.
  The morning note lists each. `zen memory accept|reject` and `POST /api/memory/{id}/review` are gone.
- The agent writes what really matters straight into `USER.md` and `IDENTITY.md` (owner's call, no
  approval round). Full files are compacted, not trimmed: the agent is told when its edit passes the
  cap, and the sleep compacts any file still over it through a session of its own
  (`turns::run_kernel_session`), checked by the kernel. Every change is backed up first in
  `~/.zenbot/backups/prompt-files/` (90 days).
- The kernel refuses `edit`/`write` on SOUL, IDENTITY, AGENTS and USER from subagents, kernel
  sessions and sessions that read untrusted content. New e2e checks and an eval task (`preference-not-memory`).

## 2026-10-08 — `zen status` sees the kernel's OpenRouter key

- `zen status` said "NO key: set OPENROUTER_API_KEY" while the key was set and the sleep was
  using it: OpenRouter's sign-in used to be reported by the retired Pi worker, and workers now
  run without the key. The kernel now reports it itself in `/api/models`.
- An OpenRouter key alone no longer counts as a signed-in engine in `zen status` or the terminal
  app (it can't run a turn). Unit test, and two e2e checks (key set and not set).

## 2026-10-08 — Every model visible; one safe continuation across providers on a hard cap (D-044)

- The model list shows every model the workers report (Haiku 5.5 included); `ZEN_MODELS` is an
  optional allowlist, and routing keeps the full catalog either way.
- On a recognized hard subscription usage limit, `zen-engine` (`failover.rs`) continues the turn
  once on the other provider (Claude → `codex/gpt-6.1-sol`, Codex → `claude/claude-sonnet-5-5`;
  `ZEN_FAILOVER_CLAUDE_TO_CODEX`, `ZEN_FAILOVER_CODEX_TO_CLAUDE`). It needs a subscription sign-in
  (refuses API-key environments), carries the prompt and the turn so far, never repeats a tool
  call whose result is unknown, shows a switch event, and stops if it can't continue safely.
  Throttles and generic 429s don't trigger it.
- Full screen keeps its scroll position while a reply streams.
- Tests: unit tests, the e2e suite (21 scenarios), and a real Claude hard cap continued on Codex.

## 2026-10-08 — One System One relevance helper; taint is its own module

- The three almost-identical System One calls for web-page focus, web-search reranking and
  knowledge-search reranking now share `score::Relevance`; result ordering, thresholds, and
  decision points stay as before.
- Untrusted-content wrapping and session taint moved from `web.rs` to `taint.rs`, because MCP,
  subagents, history and web all use it. The envelope now correctly calls all of these an external
  source (not just web). Marker defusing and the one-envelope security invariant have dedicated
  taint-module tests.

## 2026-10-08 — `web_fetch` reads Latin-1 pages

- Pages declared ISO-8859-1 / Windows-1252 (header or `<meta charset>`), or not valid UTF-8 with
  no charset, are decoded as Windows-1252 instead of showing replacement characters. Common on
  older Brazilian sites. No new dependency; unit test with Portuguese text.

## 2026-10-08 — Kernel throughput: per-session queues, paging, one summary at a time

- Worker notifications are handled in order per session, each session on its own queue, so one
  session's slow write (a turn's end, a model refresh) no longer stalls every other session's
  stream, which matters with parallel subagents. Idle queues end after a minute.
- `GET /api/sessions/{id}` takes `?last=N` and `?after=seq` (default unchanged: every message).
- A turn at the hard context limit waits for a summary already being prepared in the background
  instead of paying for a second one. Stalled turns are aborted in parallel; instruction-file
  checks run off the async runtime.
- The wiki index compares each page's file time with when it was indexed (a page restored with an
  older time is indexed again) and reads only changed pages, instead of parsing the whole wiki
  every 20 seconds.

## 2026-10-08 — Remove dead code; one place for tool permissions

- One `agent::refusal(kind, name)` decides what each kind of session may call (verifiers,
  subagents); a unit test checks it matches what each kind is offered. It also closes a gap: a
  non-verifier session could call `submit_verdict` (not offered, but not refused).
- The API no longer returns the legacy `sessions.state` (no reader); the column stays
  (expand-only). One session column list and one accept/reject parser in `api.rs`.
- Removed the unused workflow `phase` of the turn context (same text as before), the `move` arm
  for a tool that doesn't exist, and thin wrappers (`start_kernel_turn`, `append_tape`). The
  agent's SOUL path is one constant. Stale header comments and MAP notes corrected.

## 2026-10-08 — Engines and scripts: fail loud, keep secrets off argv

- Claude Code: the turn is refused if the CLI offers any tool that isn't zenbot's (checked on its
  `system/init` event; a live probe confirmed `--tools ""` lists none). The system prompt goes in a
  private file (`--system-prompt-file`) instead of argv, and the tool socket in a private per-turn
  folder instead of shared `/tmp`, removed however the turn ends. Model listing runs its
  `--version` probes off the async runtime.
- Codex: an abort stops a hung app-server start or thread setup; an aborted turn still reports its
  tokens; `codex --version` is asked once per process; history call ids never split a character.
- Workers no longer inherit the kernel's API token, database URL, OpenRouter or search keys.
- Scripts: the upgrade smoke kernel drops the OpenRouter and search keys (it ran on a copy of the
  live database and could spend money); `sleep.sh` sends the token on stdin; engine rollbacks
  quote paths with `printf %q`; SearXNG is pinned by digest (pinning the CI actions needs a token with `workflow` scope; left to the owner).
- Tested live on a dev kernel: a Claude Haiku turn with a bash tool call and a `complete`.

## 2026-10-08 — Kernel correctness: MCP, wiki, memory, workers, files

- MCP: a server whose pipe broke or that exited reconnects on the next call; a server that is down
  is retried after 60 s instead of costing its timeout on every call; a bare tool name runs only
  when exactly one server has it.
- Wiki `capture`: parallel captures are serialized; a note is skipped as "already recorded" only
  for the page System One judged; the pre-capture index covers the wiki only (no embedding call);
  new page titles and aliases stay on one line. Index passes run one at a time.
- Memory: the sleep applies all fates and decisions in one transaction, a panicking sleep no longer
  blocks later ones, sizes count characters, and the sleep removes saved outputs older than 30 days.
- Workers: a dropped request no longer leaks its entry; an invalid UTF-8 line no longer stops the
  reader; `/health` pings time out after 3 s; post-verdict work counts as busy for upgrades.
- Files: `write` and `edit` are atomic and keep permissions and symlinks; files with mixed line
  endings keep them. `history` search matches message text, not JSON; session prefixes are not
  wildcards. Skill frontmatter keeps a body's leading list dash. Secret masking reloads rotated
  sign-ins without a restart. Delegation draws are random and models refresh once per call. Child sessions (verifiers,
  subagents) no longer write their kind into the legacy `sessions.state` column.

## 2026-10-08 — Bound web caches

- `web_fetch` pages and `web_search` results now evict the oldest entry when a cache reaches 32
  entries. Both still expire entries after 15 minutes; a page keeps its 2 MB text limit.
- Why: many distinct URLs or queries could previously grow the kernel's resident memory without
  limit. A unit test checks eviction and replacement at capacity.

## 2026-10-08 — Bound kernel reads and refresh routing

- The `read` tool refuses files over 16 MiB before loading them; `edit` no longer adds a newline
  after a loose match; files under the zen home no longer attach its environment AGENTS.md as
  project instructions. Invalid or ended-turn tool requests receive errors instead of hanging.
- Worker model metadata and routes rebuild on refresh; update prebuilt probes time out and use the
  active zen home; compaction planning uses one suffix pass instead of repeated tail sums.
- Added a kernel-tools end-to-end scenario for the file-tool/context regressions. Why: prevent
  avoidable OOMs, stale model capabilities, stalls and misleading context.

## 2026-10-08 — Stop test scratch and output leaks

- Kernel unit tests now use an owner-private scratch-directory guard that removes files on drop,
  including after a failed assertion. The large-output test saves its file in that scratch home,
  not the live `~/.zenbot/outputs` folder. No production data was deleted.
- Why: repeated test runs had left hundreds of directories under `/tmp` and test logs in the live
  output folder; new runs leave neither.

## 2026-10-08 — Make the terminal client resilient and incremental

- Kept both TUI modes and `zen chat`, while splitting the interactive client into focused modules.
  Sanitize terminal text, restore terminal state on errors, report request failures without exiting,
  reload sessions after reconnect, and reset transcript state on `/new` and `/resume`.
- Full-screen rendering now caches finished stream lines and unchanged transcript entries. A 43 KB
  reply in 20-byte deltas used 0.082 s CPU in the release benchmark (previous audit: 2.08 s).
  Prompt history uses private, bounded JSON lines; client paths honor `ZEN_HOME`.
- Render/key tests cover the fixes; full release checks and the harness eval are recorded in the PR.

## 2026-10-08 — Retire Pi; System One in the kernel (D-043)

- `zend` now calls OpenRouter's System One endpoint directly, mapping bool questions to `noul`
  and recording the returned cost. A stale `ZEN_WORKERS=engine,pi` is ignored during upgrade.
- Removed `packages/mind`, its Node dependencies, `zen login pi`, the Pi updater and Pi branches
  of install, upgrade, dev and eval scripts. Claude Code and Codex still run through `zen-engine`;
  both TUI modes and `zen chat` remain.
- Tests: release build, Rust unit tests, clippy, 20/20 e2e scenarios (new local System One
  stub covers auth, model id, answer mapping and the legacy worker setting), upgrade `--check`
  smoke, and a three-task GPT harness eval (3/3 base and new). Why: remove a redundant process
  and its dependency tree.

## 2026-10-08 — Separate the development kernel's home (D-042)

- `scripts/dev.sh` now defaults the dev kernel to `~/.zenbot-dev` and moves an existing
  `~/.zenbot/dev` there once, refusing to overwrite a destination. The live home has no nested
  dev home; `ZEN_DEV_HOME` still works.
- Why: prevent dev prompt files and knowledge from appearing as live files. Checked shell syntax,
  migration behavior and the dev home contents after the move.

## 2026-10-08 — Narrow the default network and tool-output trust boundaries (D-041)

- `zend` binds loopback unless `ZEN_BIND` opts in; URL tokens work only for WebSocket upgrades.
- Shell subprocesses no longer inherit kernel secret variables; verifier token files are hidden.
  Unapproved made tools get an empty home, bounded I/O and a process-group timeout. The agent still
  runs as the owner's Unix user, so file isolation remains debt, not a solved claim.
- All tool results are masked at dispatch, after instruction-file attachments; web/MCP text is
  wrapped, and web-tainted sessions cannot rewrite active skills. Codex turns and completions use
  the same no-tools config and a private `CODEX_HOME` (only the sign-in is linked). Slow MCP calls no longer hold the
  registry lock.
- Tested: release build, unit tests, clippy with warnings denied, 19 e2e scenarios including a new
  secret-variable check; a live Codex completion with the new config. Why: reduce the blast radius
  of prompt injection before broadening agent capabilities.

## 2026-10-07 — zenbot's home laid out by scope (D-040)

- `~/.zenbot` is split by scope: `USER.md` and `AGENTS.md` stay at the top (system-wide),
  `agents/zenbot/SOUL.md` is the agent's own, and `global/` holds what every agent shares
  (`MEMORY.md`, `wiki/`, `skills/`, `tools/`). New module `crates/zend/src/layout.rs`.
- The kernel moves an existing flat layout at startup, once, before writing defaults, never
  overwriting; each old path becomes a relative symlink so a rolled-back build still finds the
  owner's files. The `zen` Files tree hides symlinks (`.` shows them).
- Tested: unit tests for the move (once, no overwrite, fresh home), e2e scenario `layout-move`
  (content, wiki history, an agent-edited skill kept, symlinks, the moved soul in the instructions,
  a second start changes nothing), the full e2e suite and the other scenarios on the new paths.
- Why: the owner wants the multi-agent layout in place now, with one agent for the moment.

## 2026-10-07 — Terminal app: restart onto a new version

- `/restart` restarts zen on the installed binary (`~/.zenbot/bin/zen`), back in the same session
  and display mode: the terminal is restored, then the process `exec`s the new zen (same PID, so
  the terminal tab stays). The kernel URL and token pass through the environment, not argv.
- `/upgrade` restarts zen by itself when the upgrade installed a new zen. When zen is upgraded some
  other way (e.g. zenbot runs `scripts/upgrade.sh` in a session), zen notices the binary changed
  (checked every 5 s) and says `/restart` loads it.
- Why: after an upgrade the owner had to quit and relaunch zen by hand to get the new client.

## 2026-10-07 — Terminal app: a navigable side panel

- The side panel is now a small dashboard: a tab strip, a **Files** tab (folder tree) and a **Viewer** tab (the open file). Ctrl+B or `/files` opens and
  closes it; Tab moves the keys between chat and panel; ↑↓ move, →/Enter open or expand, ← fold or
  go to the parent, `.` shows dotfiles, Esc/Tab return to chat, typing returns to chat too. Mouse
  clicks work on tabs and rows. `/open <file>` and `/close` still work.
- Tree logic is in `crates/zen/src/tui/files.rs` (listing is lazy, noise folders skipped).
- The tree starts at zenbot's home, `~/.zenbot` (prompt files, wiki, skills), not the directory zen
  started in; `/files <dir>` re-roots it (`~` works). Why: the owner browses zenbot's own files from
  the panel, and launching zen from `~` showed the whole home folder.
- Not built yet: a Changes (diff) tab, other tabs (wiki, sessions).

## 2026-10-07 — Terminal app: tool work folds into one line

- In full screen, a run of tool calls between pieces of text is one line (`▸ <latest step> · N steps ·
  ctrl+o`), so only the current step shows. Ctrl+O unfolds every step with its result, and folds
  them again. Inline mode is unchanged (it prints into scrollback and can't fold).
- The status line counts the running turn's tool calls and distinct files written or edited.
- Why: a long turn buried the answer under tool output; Claude Code's own TUI folds the same way.

## 2026-10-07 — Terminal app: no flicker, a side panel for files

- **No flicker** (D-039): full screen now draws each frame whole and writes only the rows that
  changed, overwriting in place (`crates/zen/src/screen.rs`). Before, every streamed word and every
  spinner tick (90 ms) cleared the bottom of the screen and repainted it, which flashed on terminals
  that ignore synchronized output (the owner's browser terminal).
- **Feels live:** the whole reply streams into the conversation, not just its last 6 lines. The
  status line shows how long the running tool has taken, so a long one (a subagent) visibly moves.
- **Side panel:** `/open <path>` shows a file next to the chat. With no path, it opens the last file
  a tool read or changed. It reloads when the file changes; `/close` closes it. The conversation
  re-wraps to fit the narrower chat.
- **Scrolling:** full screen uses the alternate screen and keeps the conversation itself: PgUp/PgDn
  or the wheel scroll it, and alt+↑↓ (alt+PgUp/PgDn, or the wheel over it) scrolls the panel.
  `/mouse` turns wheel reporting off so the terminal can select text. `--inline` is unchanged.
- Tested: 8 new render/key tests (row diffing, streaming, panel open/close/reload, re-wrap,
  scrolling), and a real run in tmux against a dev kernel with a scripted model.

## 2026-10-07 — Phase 6 built: delegation and model choice

- **delegate** (D-038): subagents with a fresh context and the same tools except `ask` and
  `delegate`; several `tasks` in one call run in parallel in the kernel; their answers come back
  (untrusted when the subagent read the web).
- **Model choice from usage:** System One classifies the kind of work; the versioned policy routes
  it; 10% exploration with the choice probability logged; the owner's verdicts are the evidence;
  the sleep switches a route only on clear evidence. `zen policy`, `zen policy set|undo`.
- **ask** with `wait: false` keeps the turn going.
- Fixed: Claude Code ran two separate `delegate` calls back to back, so one call now takes several
  tasks; measured on a real run: two Haiku subagents, same 5 seconds, both answers right.
- Phase 3 merged (#19) after its eval: 12/12 → 12/12, cost −6%.
- Tested: unit tests (route choice and exploration, switching on clear evidence only), e2e
  `delegation`, real runs on Sonnet with Haiku subagents.

## 2026-10-07 — Phase 5 built: skills and tools the agent improves itself

- **save_skill** (D-037): reason required, format and size checked, near-duplicates refused
  ("extend X instead"), new skills as drafts until the owner accepts them or an accepted session
  used them, new domains the owner's call, every change committed (skills are now a git repo).
- **save_tool**: tools the agent writes, found and called like MCP tools (`made_<name>`), run with
  JSON on stdin and a clean environment, sandboxed (no network, read-only files) until the owner
  approves them.
- The sleep flags unused skills at 30 days and archives them at 90. `zen skills` shows each skill's
  loads and the verdicts of sessions that used it; `zen skills accept|reject`, `zen tools
  accept|reject`.
- New default skill `work/close`: what to remember, capture and improve when a job ends.
- Tested: unit tests, e2e `workshop` (14 checks: draft, duplicate refused, reason required, new
  domain held, drafts found and loaded, activation by an accepted session and by the owner, the
  made tool sandboxed then approved, git history).
- Phase 3's eval re-run on Sonnet (low): Claude's monthly spend limit for the owner's org was hit
  during the first run (every turn failed on both sides); later evals use Sonnet to spare it.

## 2026-10-07 — Phase 4 built: the wiki and capture

- **Wiki** (D-036): `~/.zenbot/wiki/`, a git repository of pages (summary over a dated timeline with
  sources), with `index.md` and `log.md`.
- **capture**: System One routes a note to its page (or a new one) and skips notes already recorded
  or sensitive; secrets are masked; notes from a session that read the web are labelled `web`; every
  capture is committed. The agent writes and updates summaries.
- `search` covers the wiki (scope `wiki`); the nightly sleep commits the agent's wiki edits and
  reports pages without a summary and broken links.
- Tested: unit tests, e2e `wiki` (10 checks), and a real Sonnet session that captured two facts as
  new pages and wrote their summaries; a third, untitled note was routed by System One to the right
  existing page (0.93) and judged new (0.15 already known).

## 2026-10-07 — Phase 3 built: search over sessions and memories; long-term memory

- **search** (D-035): every session's turns (the owner's words, the answers, the tools called; not
  tool output) and every short- and long-term memory, indexed in the background; exact names and
  paths first, then full text and meaning (embeddings through OpenRouter) merged by rank; System One
  reranks; a memory found counts as used; every search logged (`searches`).
- **history** reads any session (`session`), so a session search found can be read in full.
- **Long-term memory**: reached through search. The sleep's promotions are proposals; `zen memory
  accept|reject <id>` reviews them, and promotion acts on its own once the owner's reviews show
  System One can be trusted (Wilson lower bound ≥ 0.95, about 52 agreeing reviews).
- Tested: unit tests, e2e `search` (found across sessions, exact path first, memory used, history
  across sessions, accepted promotion found as long-term), and a real Sonnet turn that found a past
  session by meaning (words not in it), reranked by System One. Fixed on the way: a match by
  meaning showed only the start of the turn, not the answer.

## 2026-10-07 — Phase 2 built: web search, web fetch, MCP; Phase 1 merged

- **Phase 1 merged** (#17) after its eval: 12/12 → 12/12, cost +27% (the larger fixed prefix).
- **Phase 0 recorded:** the research reports are in `docs/research/`.
- **web_fetch:** readable markdown with links, public addresses only (own resolver; IP literals and
  every redirect checked), 30 s / 5 MB / 20,000 characters per call with paging and a 15-minute
  cache, `focus` keeps only the relevant parts (System One), PDFs saved for pdftotext.
- **web_search:** Brave or Tavily with a key, else SearXNG (new compose service, started with the
  service and on upgrade); System One reranks results.
- **Untrusted content** (D-034): web results and remote MCP output are wrapped in an envelope a page
  can't fake and taint the session (`sessions.tainted_at`); memories from a tainted session count
  as inference.
- **MCP** (D-033): `~/.zenbot/mcp.json`; stdio and streamable HTTP; `find_tools`, `load_tool`,
  `call_tool`, so the tool list never changes mid-session.
- Tested: unit tests, e2e `mcp` (test server over stdio and HTTP) and `web` (SearXNG stub, refusals,
  taint); a real turn on Sonnet searched through SearXNG and read docs.rs with `focus`.

## 2026-10-07 — Phase 1 built: prompt files, memory, skills; the workflow out of the kernel

Merged in #17 after its eval (12/12 → 12/12, cost +27%).

- **Prompt files** (D-027): every session starts from `~/.zenbot/SOUL.md`, `AGENTS.md` (the
  environment), `USER.md`, short-term memory and a skills index. The text hardcoded in the kernel
  moved into default files the kernel writes when missing and never overwrites.
- **Tool descriptions** say what each tool does, when to use it and when not, and what it returns;
  the `<tool_guidelines>` block is gone. `move` is gone (`bash mv`).
- **Memory** (D-028): `remember` (add, replace, remove; with its source), `MEMORY.md` frozen per
  session and exported to `~/.zenbot/MEMORY.md`, a hard ceiling that refuses writes and starts a
  sleep. The sleep (nightly `zen-sleep.timer`, or `zen memory sleep`) keeps what fits, archives the
  rest, proposes long-term promotions in shadow, logs every fate in `decisions`, and leaves a note
  for the next sessions. System One judges memories unless `ZEN_S1_PRIVATE=0` (D-032).
- **Skills**: `~/.zenbot/skills/<domain>/<name>/` (agentskills.io), `find_skills`, `load_skill`;
  first skills `work/brief` and `work/verify`.
- **Workflow out of the kernel** (D-026): `flow.rs` (1,043 lines) replaced by `agent.rs` (433):
  no session states, approvals, `propose_brief`, `submit_work` or `ZEN_BRIEFS`; `verify` (kernel
  runs the commands, a fresh read-only verifier judges the rest) and `ask` stay as tools. The CLI
  lost `/go`, `/brief`, `/quick`, `/verify` and `sessions flow`, and gained `zen memory`.
- **Fixed on the way:** `history` was offered twice; memory ages decoded as the wrong SQL type
  (the turn hung); the sleep's handler could be cut off by a client timeout.
- Dev, eval and smoke kernels get their own zenbot home, so they never overwrite the live
  `MEMORY.md`. Timers are refreshed after each upgrade, so `zen-sleep.timer` arrives by upgrade.
- e2e: 12 scenarios (new: `prompt-files`, `skills`, `memory-across-sessions`, `memory-sleep`,
  `ask`; `verifier` rewritten for the tool; the workflow scenarios removed).

---

## 2026-10-06 — Kernel robustness, worker turn ids, loud engine failures, fewer queries

Merged on the owner's instruction (PRs #8, #9, #10, #13). These are harness changes; the evals their
descriptions call for were **not run** before merging, and they are not yet installed on the dev VM.
CI (build, tests, clippy, e2e) passed on each, including on the combined code.

- **Robustness** (#8): a slow summary at the hard limit no longer breaks the turn; WebSocket hubs are
  created by clients and freed when the last one leaves; the API token is checked in constant time;
  masking large output runs off the runtime's threads with a faster scan; criteria checks run on the
  bash tool's core; one async git helper, so the verifier's diff no longer blocks the runtime.
- **Turn ids** (#9, worker protocol change): workers echo the turn id, so a turn the kernel ended
  can't touch the next one.
- **Engines fail loudly** (#10) when a CLI's flags or output change under them, so the daily update
  check catches it.
- **Fewer queries** (#13): workflow state kept on the turn (no query per tool call); turn end gets the
  session's cost and parent with the turn's record; new indexes for per-session lookups (expand-only
  migration); the idle scorer no longer sorts every turn.
- e2e: 10 scenarios (new: `slow-summary`, `stale-turn`).

## 2026-10-06 — The redesign agreed; docs reorganized

No product code changed.

- **Redesign agreed with the owner** (D-025 to D-030). zenbot is reframed as a maker tool that works
  like a chief of staff. Tools, skills and context replace the kernel-enforced workflow; the prompt
  comes from `SOUL.md`, `AGENTS.md`, `USER.md` and `MEMORY.md`. Short-term memory has a nightly sleep
  and a very high bar for long-term. Skills improve without sprawl. Model choice is learned from real
  usage. New phases 0–6 (ROADMAP.md); Phase 0 is active.
- **What led there.** Model-routing experiments in real sessions (System One deciding when to switch
  model) were set aside as Engine work that doesn't make jobs better. The question became what zenbot
  is for, and the answer moved the work to tools, skills, memory and knowledge.
- **Docs reorganized** (D-031): `CONTEXT.md`, `DESIGN.md`, `MAP.md`, `DEVELOPMENT.md`, `ROADMAP.md`,
  `PROGRESS.md` and `DECISIONS.md` were added; `SPEC.md` now holds only the target modules. The
  short-lived `docs/redesign.md` (PR #14) was folded into them.

## 2026-10-06 — Engines kept current, safer upgrades, branch flow, CLI fixes

- **Engine updates** (D-022): `scripts/update-engines.sh` with a daily timer updates the Claude Code
  and Codex CLIs, tested with a real completion and rolled back on failure. Pi is pinned (1.0.4) and
  only reported. `zen status` shows the engine versions and the last check.
- **Upgrades** (D-023): migrations are tried on a throwaway copy of the live database, the install
  backs the database up first, and migrations are expand-only. `upgrade.sh` also runs a scripted Pi
  turn when Pi is enabled; `apply-upgrade.sh` logs when it restarts a kernel that never went idle.
- **Briefs opt-in** (D-021): sessions start open and the model proposes a brief only when the job
  warrants it; open sessions carry only the way into a brief.
- **Flow** (D-024): branch → pull request → green CI → merge. CI publishes binaries only after the
  checks pass, built with a smaller dist profile. `scripts/lib.sh` holds the shared script helpers;
  `dev.sh` uses its own `zen_dev` database and port; `install.sh` keeps the owner's settings and
  installs bubblewrap.
- **CLI**: times out on an unreachable kernel; sends the stream token in a header; correct token
  totals on resume; code fences survive streamed partial lines; user messages sent as a list of parts
  render; smaller binary (trimmed clap and tokio features, single-threaded runtime).
- **Docs**: SPEC, README and AGENTS described what exists (transport, layout, deployment, status).
- Installed: the service ran ee49a2f (Phases 1 and 2, briefs opt-in).

## 2026-10-05 — Phase 2: briefed work, and the debt paid

- **Briefed work** (D-020, `docs/brief.md`): frame (read-only, bubblewrap) → brief (schema-checked,
  criteria as commands) → approve → work → verify (kernel runs criteria; fresh verifier when it adds)
  → report → verdict. System One decisions in shadow mode, a `decide` tool, a routing policy table,
  `/go`, `/brief`, `/quick`, `/verify`, three eval tasks.
- **Learned:** the first build ran the workflow per message: 11/11 → 11/11 at 4.9× the cost. A session
  is one job (D-021); after that fix 12/12 → 12/12 at 2.1×. Briefs on every job don't pay for
  themselves on these tasks.
- **Debt paid:** an end-to-end suite (`scripts/e2e.sh`) and CI on every push (build, tests, clippy as
  errors, e2e); tool calls always answered; background steps count as busy and recover after a
  restart; the tape read once per turn start, missing indexes added; `zen-proto` holds the shared
  message helpers; `docs/client-protocol.md`; `main.rs` split into `api`, `turns`, `dispatch`,
  `workers`; sessions get their own workspace; one abort, one routing table, one way to read settings.

## 2026-10-05 — Phase 1: context v2

- **Context v2** (D-019, `docs/context.md`): the tape is a hash-linked chain of numbered blocks;
  instructions fixed per session, per-turn context at the end, append-only history; Claude Code
  resume and Codex thread resume with native history; every turn measures what was sent and why a
  cache couldn't be reused; summaries with block addresses and a `history` tool; secrets masked in
  tool output, full outputs kept under `~/.zenbot/outputs`.
- **Measured:** same model, 11 tasks, 8/8 → 8/8 passed (two long-session tasks newly passing), cost
  −78%, cache hit 60% → 95%. New eval tasks for long sessions and summary recall.

## 2026-10-02 — Measurement

- Full model ids, never CLI aliases (D-015); explicit thinking level per session.
- Every turn traced (D-016): harness, engine and version, model, effort, tokens, cost, time.
- `/done` records the owner's verdict (accept, more, reshape, drop).
- Evals compare the previous and the new harness on fixed tasks (D-017).
- Live scoring with a System One model (D-018, Jev via OpenRouter through Pi).
- A rolled-back build can start on a newer database.

## 2026-10-01 — The `zen` terminal app and the engines

- `zen` CLI and the systemd service; the interactive terminal app (D-011): full screen with the input
  pinned to the bottom, `--inline`, a `/` command menu, history, word motion, resize reflow, render
  tests.
- `zen-engine` (D-012): turns run on the Claude Code and Codex subscriptions with zenbot owning the
  loop and the tools.
- Robustness pass (D-013): worker supervision, abortable tools, safer edits.
- Install and self-upgrade: prebuilt binaries from CI, an update notice, `zen upgrade` / `/upgrade`.
- Project instruction files load on demand; commits made in zen sessions carry trailers (D-014).

## 2026-09-30 — Start

- SPEC v0.1, then v0.2 (D-010): three layers (Engine, Mind, Work), services first over MCP, taste,
  inbox, zen-bench, trust model.
- Walking skeleton: Rust kernel `zend`, the Pi-based model worker, a web chat (since frozen).
