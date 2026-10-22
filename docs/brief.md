# Briefs and verification

Two default skills and two kernel tools, not a kernel workflow (D-026). The agent decides when a
job needs a brief or a fresh verifier; the kernel keeps only what can't be left to the model.

- **`work/brief`** (`crates/zend/defaults/skills/work/brief/`): for a big, risky or unclear job,
  name the route (quick, bounded, architectural), research before asking, ask only the owner's
  questions, write a short brief (intent, goal, scope, assumptions, criteria as commands where
  possible), and wait for the owner's go-ahead only on architectural work.
- **`verify`** (`crates/zend/src/agent.rs`): the kernel runs every criterion that has a command
  (`run`, optional `expect`) in the contained shell. When some criteria need judgment, or `fresh`
  is set, and no command failed, a child session of kind `verifier` on the same model, with no
  history, gets the goal, criteria, command results, the maker's summary (a claim) and the diff
  from `base` (default `HEAD`), with only `read`, a read-only `bash` and `submit_verdict`. Its
  prompt is `crates/zend/steps/verify.md`. The result is a `verification` tape block.
- **`ask`**: 1–3 questions, 2–4 options each, recommended first; the kernel records a `questions`
  block and refuses further tool calls in the turn unless `wait: false`. Job and subagent
  sessions have no `ask`.

The kernel-enforced workflow of D-020 was removed on 2026-10-07; its columns and tape kinds stay
unwritten (expand-only migrations). Skill loads and `verify`/`ask` calls are `tool_calls` rows, so
their use can be compared against the owner's verdicts.
