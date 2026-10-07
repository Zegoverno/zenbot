# Briefs and verification

How zenbot frames a job and proves it's done: two skills the agent loads when they help, and one
tool the kernel runs. There is no workflow in the kernel (D-026): the agent decides when a job needs
a brief and when work needs a fresh verifier. The kernel keeps only what can't be left to the model:
the criteria's commands run in the kernel, a verifier never sees the maker's reasoning and can't
change anything, and a question to the owner ends the turn.

This replaced the kernel-enforced workflow of D-020 (frame → approve → work → verify → report, with
session states, `propose_brief`, `submit_work` and approvals) on 2026-10-07. Measured before the
change: briefs on every job gave the same pass rate at about twice the cost (D-021, D-026).

## The `work/brief` skill

`~/.zenbot/global/skills/work/brief/SKILL.md` (default in `crates/zend/defaults/`). For a job that is big,
risky or unclear:

1. **Find the real job.** Say the route: `quick` (just answer), `bounded` (a change with a clear
   shape), `architectural` (several approaches or a design to choose); when in doubt, the heavier
   one. A solution the owner proposes is a hypothesis about the goal.
2. **Research before asking** (Codex plan mode, BMAD).
3. **Ask only what's the owner's**, with the `ask` tool: at most 3 questions, 2–4 options each,
   recommended first (Spec Kit, Codex `request_user_input`). Everything else becomes an assumption.
4. **Write the brief** (template in `references/template.md`, about 1,000 tokens as in BMAD):
   intent (stated, quoted; assumed), goal (current → target, GSD), scope and must-nots, assumptions,
   criteria as commands with expected output where possible.
5. **Do the work.** For an architectural job, show the brief and wait for the owner's go-ahead;
   otherwise carry on. Decide unclear points and note them for the report (Superpowers' rulings).
   When done, load `work/verify`.

## The `work/verify` skill and the `verify` tool

The skill says when a fresh verifier adds something (judgment criteria, risky or architectural
work, a long series of attempts) and when it doesn't (every criterion is a command that passes).

`verify { goal, criteria: [{ text, run?, expect? }], dir?, base?, summary?, fresh? }` (`agent.rs`):

1. **The kernel runs every criterion that has a command**, in `dir` (default: the workspace), and
   records pass or fail from the exit code and, with `expect`, the output.
2. **A fresh verifier**, when some criteria have no command or `fresh` is set, and no command
   failed (a failed command's output is the evidence). It is a child session (`kind = 'verifier'`)
   on the same model, with no history: it gets the goal, the criteria, the command results, the
   maker's summary (a claim, not evidence) and the diff from `base` (default `HEAD`, so uncommitted
   changes), and only `read`, a read-only `bash` (bubblewrap) and `submit_verdict`. Its prompt is
   `crates/zend/steps/verify.md`. It answers `pass`, `fail` or `uncertain` per criterion with
   evidence; weak or indirect evidence is not a pass (Codex's completion audit). It can't override a
   failed command. GSD measured why it must not grade its own work.
3. The result goes back to the model as the tool's result (an error when anything failed) and onto
   the tape as a `verification` block. The model fixes and checks again; the skill caps that at two
   more rounds before reporting.

## Asking the owner: `ask`

`ask { questions: [{ question, options }] }`: 1–3 questions, 2–4 options each, the recommended one
first. The kernel records a `questions` block, shows them to the owner (`questions` event) and
refuses any further tool call in that turn; the answers arrive as the owner's next message. An
unanswered question means the recommended option, reported as an assumption.

## Measurement

Every load of a skill and every `verify` and `ask` call is a row in `tool_calls`, and their results
are on the tape, so how often briefs and verifiers are used, and the verdicts (`/done`) of sessions
that used them, can be compared with sessions that didn't. Database columns of the old workflow
(`sessions.state`, the `brief`, `state`, `submission`, … tape blocks, `policies`) are kept
(expand-only migrations) and no longer written.
