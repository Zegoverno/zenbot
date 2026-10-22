# zenbot — Superseded decisions (archived)

> Moved unchanged from DECISIONS.md on 2026-10-14 (D-061): decisions a later one superseded.
> DECISIONS.md keeps a one-line entry for each, linking here. Newest first.

## D-028 — Memory: fixed-size short-term, nightly sleep, a very high bar for long-term

> Long-term promotion superseded by D-045 (2026-10-08): the agent writes traits and guidance into
> `IDENTITY.md` or `USER.md` itself, and the sleep keeps, archives or promotes into them and the
> wiki. Paths moved by D-040.

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

**Superseded by D-045 and D-069** (moved here 2026-10-21).

---

## D-021 — A session is one job; briefs opt-in

**Decision:** The briefed workflow runs once per job, not per message. Briefs are opt-in (`ZEN_BRIEFS`,
default `opt-in`): the model proposes one for a big, risky or unclear job; the owner's `/brief`
forces one. Their value is judged from real use (verdicts and cost, briefed vs unbriefed), not from
synthetic harder evals.

**Why:** The first Phase 2 build re-framed and verified per message (4.9× the cost). With the job
model, briefs on every job still cost 2.1× for the same pass rate.

**Superseded in part by D-026** (briefs become a skill; `ZEN_BRIEFS` and `/brief` are gone).

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

## D-008 — Channels

**Decision:** Web UI first, Matrix later. (Web-first superseded by D-011; Matrix built in D-049.)

**Date:** 2026-09-30

---

## D-005 — Rust kernel, TypeScript model worker

**Decision:** Rust kernel (`zend`) + TypeScript model worker (`zen-mind`, using Pi) + TypeScript web
UI.

**Why:** The always-on parts benefit from Rust's safety, footprint and single binary; the LLM
ecosystem (Pi, Claude Agent SDK, MCP SDKs) is TypeScript-first; LLM latency dominates, so the loop's
language doesn't affect speed.

**Superseded:** the TypeScript worker by D-012 (`zen-engine`, Rust) and D-043 (Pi retired); the
TypeScript web UI by D-011 (the `zen` terminal app; the web UI was frozen, then removed on 2026-10-10).

**Date:** 2026-09-30

## D-036 — The wiki: pages over timelines; System One routes, the agent writes

**Superseded in part by D-070** (legacy wiki writer and made-tool paths).

> The wiki moved to `~/.zenbot/global/wiki/` (D-040).

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

## D-006 — Postgres + pgvector · Superseded by D-076

**Decision:** Local Postgres with pgvector via `DATABASE_URL`, movable off the VM later.

**Date:** 2026-09-30

---
