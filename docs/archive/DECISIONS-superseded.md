# zenbot — Superseded decisions (archived)

> Moved unchanged from DECISIONS.md on 2026-10-14 (D-060): decisions a later one superseded.
> DECISIONS.md keeps a one-line entry for each, linking here. Newest first.

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
