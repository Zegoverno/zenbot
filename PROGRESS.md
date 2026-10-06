# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

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
