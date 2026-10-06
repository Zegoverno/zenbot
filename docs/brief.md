# Briefs, work and verification

How a zenbot session turns a job into verified work: **frame → approve → work → verify → report →
close**. **A session is one job.** The workflow runs once per session, not once per message: framing
spans as many turns as it takes to understand the job (clarifying, researching, questions,
misconceptions), the work spans as many turns as it takes (the owner's messages steer the same job),
the job is verified once when it should be done, and after the report a reply continues the same job.
A new job is a new session. The model can run every step end to end; the owner can take any step himself.
Every step is recorded and measured, so the loop can be improved from real use and evals.

Approaches are taken from the best of the projects studied (Superpowers, Spec Kit, GSD, BMAD,
Kiro, Claude Code plan mode, Cline, Roo Code, Codex, Aider), checked in their code or official
docs. None of them measures whether planning helps, and almost none enforces its gates: their
gates are instructions the model may ignore (Cline added a shell blacklist after models edited
files from plan mode). zenbot runs every tool in the kernel, so the gates that matter are code.

## Opt-in (ZEN_BRIEFS)

Briefs are **opt-in** by default (`ZEN_BRIEFS=opt-in`): a session starts `open`, the single loop,
and the model proposes a brief when the job is big, risky or unclear; `/brief` makes the next
request start with one. From the moment a brief is proposed, the session follows the workflow below
(nothing changes until it is approved). `ZEN_BRIEFS=always` starts every session by framing;
`ZEN_BRIEFS=off` removes briefs. Why opt-in: on the evals, briefed work passed as often as the single
loop at about twice the cost (after making it follow the job, it had been 4.9×), and synthetic
"harder" tasks are what models are best at. Whether briefs pay off is judged from real use: the
owner's verdicts and the cost of briefed and unbriefed sessions.

## Session states

| State | The model can | Leaves when |
|---|---|---|
| `framing` | read files, run read-only commands, ask questions, propose a brief, answer directly | it answers without changing anything (`quick`), or a brief is approved |
| `working` | everything; asks only to stop on the four conditions below | it submits the work |
| `verifying` | (kernel and verifier only) | criteria are checked |
| `reported` | | a verdict is recorded (`more` goes back to `working`, `reshape` to `framing`), or the owner replies (back to `working` on the same brief) |
| `closed` | | the owner replies (back to `working` on the same brief) |

States are blocks on the tape (`state`, with who moved it: `model`, `owner` or `kernel`, and why).
The owner can move a session himself: `/brief` (frame now), `/quick` (skip the brief), `/go`
(approve), `/verify`, `/done`.

## Framing

A briefed session starts in `framing` (or reaches it when the model proposes a brief). The procedure (`steps/frame.md`, loaded only in this state):

1. **Route out loud**: `quick` (a question or a look: answer it, no brief), `bounded` (a change with
   a clear shape), or `architectural` (several approaches or a design to choose). When in doubt,
   the heavier route (Superpowers).
2. **Investigate before asking.** Never ask what the files, history or memory can answer (Codex plan
   mode, BMAD).
3. **Ask only what blocks**: public, security, data, money or hard-to-reverse choices, or a goal that
   can't be inferred (caveman's rule). At most 3 questions per round, each with 2–4 options and the
   recommended one first, so "yes" accepts them all (Spec Kit, Codex `request_user_input`).
   Everything else becomes an **assumption** written in the brief, which the owner can veto.
4. **Propose the brief** with `propose_brief`, which ends the turn.

Read-only is enforced, not requested: in `framing` the kernel refuses write tools when they're
called, and `bash` runs in a read-only sandbox (bubblewrap: the filesystem is mounted read-only,
`/tmp` is private). From the moment a brief is proposed, the model is offered the same tools and instructions in every
phase (both procedures are in them; the turn context says which phase applies), so the prompt cache
holds from framing into the work. An open session carries only a short note on how to opt in and
the propose_brief tool: unbriefed sessions don't pay for the workflow (measured: the full
procedures and tools on every session cost small jobs 27–61% more).

## The brief

A block on the tape (`brief`, versioned: a reshape writes a new version pointing to the previous
one). The kernel checks it against the schema; `propose_brief` fails with the reasons if it doesn't
pass. Target size about 1,000 tokens (BMAD); over 2,000 is rejected with "split or cut".

| Field | Holds |
|---|---|
| `route` | quick, bounded, architectural |
| `work` | the kind of work: understand, shape, bet, build, verify, maintain, reflect, reach |
| `intent` | `stated`: what the owner said, quoted; `assumed`: what the model inferred |
| `goal` | current → target, in one or two lines (GSD) |
| `scope` | in, out (each with a reason) |
| `must_not` | boundaries, verbatim (BMAD's "Never", GSD's must-NOT) |
| `questions` | each question asked and its answer |
| `assumptions` | defaults taken instead of asking |
| `context` | files and repos to load; for a repo, its path (the diff is taken there) |
| `criteria` | each `{ id, text, run?, expect? }`: a command and its expected result where possible (`cargo test --release`, a `zen ask --json` faux turn, a grep), else a judgment for the verifier |
| `appetite` | how much it's worth: time and cost |

Once approved, `intent`, `goal`, `scope` and `must_not` are frozen: only the owner changes them (a
reshape). The work can't redefine success.

## Approval

`propose_brief` ends the turn (Cline's `completesRun`). The brief is then approved by:

- the owner (`/go`, or "yes"), or
- **auto-approve**, set per route (`ZEN_AUTO_APPROVE`, default `quick,bounded`): the kernel approves
  and continues at once.

On approval the kernel records the git state of each repo in `context` (the baseline for the diff),
picks the model for the work (the routing policy, below), and starts the work:

- **quick and bounded**: in the same context, so the files framing read stay cached; the brief
  arrives as a message ("treat it as the source of intent").
- **architectural** (`ZEN_FRESH_CONTEXT`): in a **fresh context** whose instructions include the
  brief (Codex's clear-context handoff, Aider's editor). The framing chat isn't replayed; the
  `history` tool still reads it.

Measured on a small change (Opus, add a function and a test): a fresh context and an always-on
verifier cost 3.8× a session without briefs ($0.181 vs $0.048, 36s vs 8s); continuing the context and
running the verifier only when needed brought it to 2.2× ($0.104, 18s), with the work turn reading
21k tokens from the cache.

## Working

The normal turn loop, with the brief in the instructions. Two rules (Superpowers):

- **Rulings, not stalls.** An unclear point is decided and recorded with `note_ruling { what, why,
  cost_if_wrong }`; rulings are shown in the report.
- **Stop and ask only** for something irreversible, security-sensitive, outward-facing (push,
  publish, send, spend), or when the brief is so wrong that every path is a guess.

The model ends the work with `submit_work { summary }`, which ends the turn and starts verification.

## Verification

1. **The kernel runs every criterion that has a command**, in the brief's repo, and records pass or
   fail from the exit code and output. Nobody studied does this mechanically; GSD comes closest.
2. **A fresh verifier**, when it adds something: when a criterion has no command, for architectural
   work, or for a sample (`ZEN_VERIFY_SAMPLE`, default 0.2) so its value keeps being measured; not
   when every criterion is a passing command, and not when a command failed (the work goes back
   with its output). It is the same model in a new engine session with no history, and gets only the
   brief, the diff since the baseline, the command results and the rulings, plus read-only tools.
   It answers each criterion `pass`, `fail` or `uncertain` with evidence, through a tool with a
   fixed schema (Codex's completion audit: weak or indirect evidence is not a pass). It never grades
   its own work, and it can't override a failed command. GSD measured why: a verifier asked to judge
   what it couldn't check passed it every time; telling it to abstain only brought that to 67%.
3. Anything failed goes back to `working` with the findings, up to `ZEN_VERIFY_ROUNDS` (default 2);
   then the session is reported as it is.

## Report and close

The report leads with what needs a decision: criteria (pass, fail, uncertain), rulings, assumptions,
files changed, cost and time against the appetite. Then a verdict: accept, more, reshape or drop.

- The owner's `/done` is the ground truth.
- With auto-close (`ZEN_AUTO_CLOSE`, default off for `architectural`), the model records its own
  verdict. Every verdict records its source (`owner` or `model`); an owner verdict replaces the
  model's. How often the model's verdicts match the owner's is itself a metric.

## System One decisions

A System One model (Jev, via `s1.decide`) answers typed questions where a decision is frequent,
short and cheap to correct. Each decision is logged in `decisions` with its input, answer and
probability, and later what actually happened (the model's route, the owner's override, the
verdict), so it can be calibrated. They start in **shadow mode**: logged, not acted on, until their
agreement with what happened is good enough.

| Where | Question |
|---|---|
| First message | route (quick, bounded, architectural); kind of work |
| Approval | which model and thinking level (from the routing policy) |
| Verification | does the work claim success without evidence? (triage: whether to look harder, never whether it passed) |

The model also gets a `decide` tool to ask a System One model typed questions itself (bulk triage,
ranking, a cheap second opinion), logged the same way; `ZEN_DECIDE_TOOL=0` turns it off.

## Routing policy

Which model and thinking level do the work, per kind of work: a versioned record (`policies`),
starting as "the default model for everything". Day-to-day policy changes when the data shows a
better fit (Haiku where it's enough, Opus for deep research, a new model that fits better); every
change is logged with its evidence and can be undone. System-level changes (code, architecture,
the improvement loop itself) are proposed to the owner. Phase 2 stores and applies the policy and
logs every decision and outcome; Phase 2b adds the loop that changes it (below).

## Measurement and evals

Per session: route and work type (model's, Jev's, owner's override), questions asked, brief size,
approval (auto or owner) and time to it, rulings, criteria results (command and verifier), verify
rounds, cost and time against appetite, verdicts by source.

New eval tasks (the runner answers questions with the task's `answers`, or "take your recommended
defaults"):

- an incomplete request: it must ask before acting, and change nothing while framing;
- a clear request: no unnecessary questions, criteria pass;
- a small change to a copy of a project with tests: criteria pass when verified by the kernel.

## Phase 2b: the improvement loop

1. **Sweeps**: run the eval tasks (tagged by kind of work) across candidate models and thinking
   levels; per kind of work, the cheapest that passes as often as the best.
2. **The improver**: a zenbot session (kind `reflect`) that reads the metrics from real sessions and
   sweeps, proposes policy changes with evidence, applies day-to-day ones (logged, undoable), and
   writes system-level proposals for the owner.
3. **Simulated usage**: eval tasks generated from real sessions, so the evals follow how zenbot is
   actually used.
