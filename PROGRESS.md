# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

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
