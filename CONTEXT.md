# zenbot — Context

> Read this first. It says what zenbot is for, how success is measured and what constrains it.
> Everything else follows from here: [ROADMAP.md](ROADMAP.md) for what's being built now,
> [DESIGN.md](DESIGN.md) for how the system works, [SPEC.md](SPEC.md) for the modules it is meant
> to have, [DECISIONS.md](DECISIONS.md) for why things are the way they are.

## What zenbot is

A **maker tool that works like a chief of staff.** The owner stays the CEO of their own life; zenbot
carries the jobs they hand it, end to end, so their attention goes to the frontier of knowledge, gut
calls and taste: the decisions that really matter.

- **The owner drives.** They start every job and decide where tokens are spent. zenbot never starts a
  job on its own (for now). Inside a job it may split the work into subtasks and delegate them.
- **Any kind of job.** Operational work and building software, across the owner's job, projects and
  own companies.
- **A second brain.** It takes tasks, memory and thinking off the owner's plate: what they know, what
  they decided and why, where things stand.

## How success is measured

How well zenbot does a job end to end:

1. identifies the real job to be done, not only the literal request;
2. researches all the context it needs (code, memory, data, the web);
3. decides what to build and what is relevant;
4. brings the owner only the questions and decisions that are theirs;
5. solves it to a state-of-the-art standard.

The owner's verdict on each job (`/done`: accept, more, reshape, drop) is the ground truth.
Supporting numbers:

- jobs accepted without edits, per kind of work; cost and time per accepted job;
- questions asked, and how many only the owner could answer;
- share of recurring work running as scripts or skills instead of pure LLM;
- memories, skills and wiki pages that are used, and that the owner reads and approves;
- eval pass rate and cost trend (a regression check, not the measure of quality).

## How it gets there

Give the agent **tools, skills and context**, not a fixed workflow. A capable model with the right
tools does better on open-ended jobs than a fixed sequence of steps. zenbot owns the prompt files,
tools, skills, memory and knowledge; any model can do the work. Fixed rules exist only for:

- **authority:** the owner starts jobs and sets the budget; the agent never decides the owner's calls;
- **safety:** every action runs through the kernel, sandboxed, secrets masked, audited;
- **measurement:** the tape records everything, so work can be judged and improved.

## Principles

1. **Payback first.** What ships is used on real work the week it ships. Thin vertical slices, not
   complete modules.
2. **State of the art, grounded.** Every design choice follows the best known practice, checked in
   reference projects' code (not their READMEs), and says where it comes from, where zenbot goes
   further, and what it means for security.
3. **Provider-free.** Claude Code and Codex on the owner's subscriptions, System One classifiers
   such as Jev through OpenRouter; always the latest models. Engine-native features are used only behind a
   contract with a zenbot-owned fallback.
4. **Only what the job needs enters the context.** Instructions and system tools are fixed per
   session; skills and other tools load on demand, appended so the prompt cache holds.
5. **Composable.** Every building block is a module with a documented contract; switching an
   implementation is a config change, not a rewrite. Adopt good open source before building.
6. **Open contracts.** MCP for tools · OpenAI/Anthropic-compatible HTTP for models · JSON-RPC between
   kernel and workers · markdown + git for knowledge · Postgres via `DATABASE_URL` for state.
7. **The kernel owns state; workers are stateless.** All side effects go through the kernel, under one
   permission model.
8. **Deterministic where possible.** Repeatable LLM work is distilled into scripts over time.
9. **Measured, not assumed.** Every model call, tool call and cost is traced. Evals compare harness
   versions and inform the owner's decision; they are never an automatic gate.
10. **The owner's attention is the scarcest resource.** Interrupt only when needed; every question
    comes with a recommendation and a default.
11. **Own your data.** Everything is exportable as plain files; nothing important lives only in a
    database.
12. **Host-agnostic.** Runs on any Linux box with Docker; exe.dev is only where it runs today.

## Constraints

- **One owner, one solo builder.** zenbot is a personal tool, not a product. It is developed in public
  (github.com/Zegoverno/zenbot).
- **Subscriptions first.** Models run on the owner's existing Claude and ChatGPT plans through
  officially supported paths; a small, capped API budget covers System One and embeddings.

- **zenbot improves zenbot.** Its own development is the first real workload (dogfooding).

## Thesis

As AI makes building cheap, value moves to speed, uniqueness and taste. Expect many tiny companies
solving very niche problems, often serving agents. Agents won't buy niche code (they can regenerate
it); they buy what they can't cheaply regenerate: proprietary or fresh data, precomputed results,
access, accountability and judgment. zenbot helps one person accumulate knowledge and taste and turn
them into working companies.

## What zenbot is not

- Not a product, and not built for other users.
- Not an autonomous agent that decides what to work on.
- Not a fixed workflow engine: steps run when they add value, not because a script says so.
- Not tied to one model provider.
