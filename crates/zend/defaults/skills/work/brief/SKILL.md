---
name: brief
description: Frame a big, risky or unclear job before changing anything - find the real job, research, ask only what's the owner's, and write a short brief with checkable success criteria. Use when a request is large, ambiguous, touches many files or has several possible approaches; skip it for small, clear jobs.
---

# Brief: frame the job before doing it

A session is one job. Framing is worth it when the job is big, risky or unclear; for a question or a
small, clear change, just do it.

## 1. Find the real job

- Say the route in one line: **quick** (a question or a look: just answer), **bounded** (a change
  with a clear shape), or **architectural** (several approaches, or a design to choose). When in
  doubt, take the heavier route.
- Treat a solution the owner proposes as a hypothesis: the goal is the problem it solves.

## 2. Research before asking

Read the code, docs, memory and history that bear on the job; search the web when the answer is out
there. Never ask what you can look up. Watch for stale or one-sided evidence.

## 3. Ask only what's the owner's

Ask about taste, the bet, and anything public, security-sensitive, costly or hard to reverse, or a
goal you can't infer. Use `ask`: at most 3 questions, each with 2 to 4 options, your recommended
option first. Take a sensible default for everything else and list it as an assumption.

## 4. Write the brief

Write it to a file in the repository you'll change, or in your reply when there's no repository
(about 1,000 tokens; template in `references/template.md`):

- **Intent**: what the owner said (quoted) and what you inferred.
- **Goal**: current state → target state, in one or two lines.
- **Scope**: in, out (with why), and must-nots.
- **Assumptions**: defaults you took instead of asking.
- **Criteria**: how you'll know the whole job is done, as commands with the text their output must
  contain where you can (a test, a build, a grep, `zen ask --json`), judgment criteria otherwise.

For an architectural job, show the owner the brief and wait for their go-ahead before changing
anything. For a bounded job, carry on.

## 5. Do the work

- The brief is the source of intent: don't redefine success midway. The owner's messages steer the
  same job.
- Decide unclear points yourself; note each decision (what, why, cost if wrong) for the report.
- Stop and ask only for something irreversible, security-sensitive or outward-facing, or when the
  brief turns out so wrong that every path is a guess.
- When the whole job is done, check it: load the `work/verify` skill.
