# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

## 2026-10-19 — Phase 2b and Phase 3: approved jobs and kernel recovery (D-068)

Owner-approved jobs retain their named sources and workspace; agent edits reset that authority.
Source judgments distinguish identifiable publishers from unknown ones, and aligned file edits
can proceed at high risk. Held jobs wait for owner queue decisions and then finish their reports.
MCP stdio servers use containment and their own egress policy; status no longer connects them.
`zen trust suggest` proposes entries for review. Busy prompts queue durably in the kernel;
`/steer` and Matrix `!steer` interrupt at the next tool boundary. Worker writes are bounded,
tool bookkeeping errors return a result, summary flags clear on task exit, and nested permission
checks share an invalidated snapshot. JSONL session mirrors retry from a durable export queue.
Legacy workflow blocks no longer hide history. Validation is recorded in the PR before merge.

## 2026-10-16 — Security: context risk, action alignment, contained sessions (D-067)

Every shell now runs contained (no secrets, no sudo, no host sockets) with its network through a
per-session egress proxy in the kernel; acting as the owner is a `host: true` command the gate
checks. One S1 decision (`action_gate`) runs, holds or locks each effect from the session's context
risk, the action's effect and, only when needed, alignment with the owner's job; most actions need
no model call. The owner's trust list (`zen trust`, `~/.zenbot/trust.toml`) covers URLs, APIs, MCP
servers and repos by remote. Rating fixes: legacy marks reset (migration 0033), clones rated by
their remote, engine errors and medium products no longer raise risk. Removed: the shell grammar,
the SafeRead/offline modes as policy, the task class table. Tests: unit (classify, rules on inputs,
contained-shell enforcement, proxy parsing, trust parsing), e2e `security` (proxy end to end, holds,
owner allow, no sudo/Docker/token), the gate scenarios rewritten; replay: 7,263 of 7,284 low-risk
actions run without a model call, none held by rule.

## 2026-10-16 — Unapproved made tools can't reach host sockets

An unapproved made tool's sandbox had no seccomp filter and kept `/run`, so it could connect to
the Docker socket (root on this host) despite having no network: a risky session could save a tool
and run it. It now gets an empty `/run` and the offline shell's filter (no connect or send on any
socket). Test: a socket in the tool's own folder is refused, `/run` is empty.
## 2026-10-16 — S1: System One as one engine of decision files (D-066)

Every System One call now goes through `crates/zend/src/s1/`: thirteen decision files
(`crates/zend/s1/*.toml`; the owner's `~/.zenbot/s1/` overrides) with typed questions, a policy by
costs or rules, approved models (`~/.zenbot/s1/models.toml`, else `ZEN_S1_MODEL`), timeouts, a
circuit breaker, masked states and one ledger. Behaviour is unchanged: today's thresholds became
costs and rules, checked by each file's test cases. New: `zen s1` (numbers per decision), `zen s1
check` (line and column), `models`, `replay`, `compare`, `proposals`/`approve`/`reject`; the
brain's `judge` tool replaces `decide`; the owner's queue answers and job choices label
decisions, and the nightly sleep fits calibration curves and proposes changes. Removed: session
scoring and the subagent kind classifier (`score.rs`), `ZEN_DECIDE_TOOL`, `ZEN_SCORE_IDLE_SECS`;
`ZEN_JOB_BAR` is deprecated. Tests: unit (spec, checker, rules, policy math, breaker, masking,
calibration), every default file through the checker and its tests, e2e `system-one` (judge,
ledger, a broken owner file, System One down) and the existing scenarios unchanged.

## 2026-10-16 — D-065 security corrections (branch, not installed)

Plain Matrix reactions no longer authorize; encrypted cross-signed owner commands remain available.
Project-controlled execution retains sandbox/hold capabilities at risks 2–3, independent of relevance.
Owner authority is never prefix-truncated: oversized goals fail closed. Private content scoring
honors opt-out. Reusable assessed publisher scopes exclude GitHub public contributions and page-only
ratings cannot propose broader prefixes. Validation below predates these corrections; focused results
are in the commit. Fresh focused action-gate/source-task-queue e2e passes (0 failed checks);
disposable source/cache/task/queue DB regressions and private-opt-out checks pass, as do the five
Matrix authentication-policy tests. Matrix transport e2e was updated but not rerun after these fixes.
The earlier mixed-revision full check failed five checks (two addressed by this focused rerun,
three delegation/wiki checks outstanding and not rerun). No completed full rerun or installation claimed.

## 2026-10-16 — Integrated D-065 source/task/queue validation (branch, not installed)

Authenticated task checks now integrate with terminal queue decisions and source ratings. Safe
reads/local data writes skip alignment; persistent/outward/arbitrary-code effects fail closed.
Rejections cannot be revived through legacy allow or requeued. The e2e runner includes disposable
queue/task DB regressions and source-to-task-to-queue turns with controlled classifier failures.
Restore the level-4 web review marker alongside numeric provenance; Matrix validates actual queue
notices and owner approve/reject/deny reactions, including exact-once effects and contamination.
Validation results are recorded with the integration commit; no live install is claimed.

## 2026-10-16 — Owner approval queue (D-065 D)

Unified authenticated queue routes list held actions and exact source proposals. Atomic decisions
grant once, refuse without retry, or explicitly deny/contaminate; idle continuations are kernel
origin, not owner messages. Terminal picker/CLI and Matrix owner-only reactions share the routes.
Matrix posts to existing private rooms and persists exact event mappings for replay safety.

## 2026-10-16 — System One source metadata and risk 2–3 admission (D-065 A–B)

Metadata-only judgments are cached for 30 days by exact URL path, keeping public contributions
separate from maintained code. Search misses are batched; a separate content check can only raise
risk and unavailable checks fail closed. Owner allowlist grants remain separate from inferred
reputation. Atomic, snapshot-bound proposal interfaces await the owner queue. Ratings through 3
enter instructions with labels and writer dependencies; local work stays normal at 2–3.
Re-rating authenticates exact stored web bytes and lowers stored rows only with explicit
writer-only evidence; ambiguous historical provenance stays unchanged. Expand-only migration 0030.

## 2026-10-16 — Retained-byte provenance after file copies

Aggregate retained file provenance per writer before upserting an edited version. Multiple paths
with the same bytes no longer target one upsert row twice; maximum risk and writer dependencies
are preserved. An isolated database regression covers copy, edit and repeated edit.

## 2026-10-16 — Origin ratings, deny/trust by cause, masked shells (D-062 step 3, D-064)

Stored items (files by exact bytes, memories, reports, skills, made tools, project instructions)
keep their origin rating and writer edges; only 0–1 enters instructions. The owner's `/deny`
(`!deny`) stops a session and everything that read its products; `/trust` lifts only reviewed
causes. Shells see files from denied sessions masked by content digest (bubblewrap, like Anthropic's
sandbox-runtime; copies and hard links too; masked host shells get no Unix sockets), bash results
take the rating of rated files they show, host-shell products are registered and a concurrent
deny withholds output. `/allow` and `/deny` now work in the terminal app (`/allow` was
missing). Codex native subagents are off (`agents.enabled = false`). Tests: unit, 14 database tests
(races, descendants, stale trust, forged provenance, shell masks), the `origin` e2e scenario.

## 2026-10-15 — Offline, read-only project code by default (D-063)

Risky sessions run project code without network or workspace writes; private `/tmp` remains
writable. Attempts offer exact owner `/allow` approval without tainting on that basis. An exact
verification grant also reaches its criteria. Constrained file tools keep their existing policy.
No migration or unfinished D-062 step-3 implementation is included.

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

Older entries: `docs/archive/PROGRESS-*.md`.
