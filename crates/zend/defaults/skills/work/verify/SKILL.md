---
name: verify
description: Check that a job is really done before reporting it - run the criteria yourself, then get a fresh verifier's judgment with the verify tool when the stakes or the judgment calls justify it. Use before telling the owner a change is finished, fixed or working.
---

# Verify: prove it before you say it

Never claim something is done, fixed or working without evidence you checked.

## 1. Check it yourself

Run what proves the job is done: the tests, the build, the command that shows the behavior, a
`zen ask --json …` against a running service. Read the output; a command that ran is not a command
that passed.

## 2. Decide whether a fresh verifier adds something

Call the `verify` tool when any of these hold:

- some criteria need judgment (design quality, completeness, "matches what the owner asked");
- the change is architectural, risky, or hard to reverse;
- you went through several attempts and may have lost sight of the goal.

Skip it when every criterion is a command that passes: the commands are the evidence.

## 3. Call `verify` well

- `goal`: the job's goal in one or two lines.
- `criteria`: each with `text`; give `run` (a shell command) and `expect` (text its output must
  contain) wherever a command can check it. The kernel runs those itself.
- `dir`: the repository the work changed (the verifier sees its diff).
- `base`: the git commit the work started from, if you committed along the way.
- `summary`: what you did and how you checked it. The verifier treats it as a claim, not evidence.

The verifier is a separate session that never sees your reasoning. It can only read files and run
read-only commands.

## 4. Act on the result

- Fix failures, then check again (at most two more rounds; then report what still fails).
- Report to the owner: what passed and failed, decisions you made on your own, assumptions. Put
  what needs their attention first.
