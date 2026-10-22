# Context

How a turn's context is built, stored, measured and sent. Goals: most of each request served from
the provider's prompt cache, sessions that never overflow, nothing lost to summaries, every turn
measured.

## The request

From what never changes to what changes every turn, since a provider caches only an identical prefix:

| Piece | Changes | Holds |
|---|---|---|
| Tools | per session | the kernel's tools, fixed order |
| Instructions | per session | prompt files, short-term memory, skills index, workspace AGENTS.md files |
| Summary | rarely | older turns, citing the blocks they came from |
| History | appended to | messages since the summary, word for word |
| Turn context | per turn | the date when it changed |
| Prompt | per turn | what the owner typed |

**Instructions** (`compile::system_prompt`, D-027), in order: `~/.zenbot/agents/zenbot/SOUL.md`,
`~/.zenbot/agents/zenbot/IDENTITY.md` (D-045), `~/.zenbot/AGENTS.md` (the environment; `{{workspace}}`,
`{{home}}`, `{{zen_home}}`, `{{repo}}` filled in), `~/.zenbot/USER.md`, short-term memory as at the
session's start (`[m12] …`, with a note on last night's sleep and upkeep findings), the skills index
(names and descriptions, or only domains past `ZEN_SKILL_INDEX_CHARS`), the instruction files
(`AGENTS.md`, else `CLAUDE.md`) from `/` down to the workspace, and the working directory. A job's
session gets only the parts its `context` names (`compile::PARTS`; the soul always). Caps:
`ZEN_SOUL_CHARS` 4000, `ZEN_IDENTITY_CHARS` 4000, `ZEN_AGENTS_CHARS` 12000, `ZEN_USER_CHARS` 3000;
a longer file keeps its first 70% and last 20% with a note where to read the rest. Missing defaults
are written at start from `crates/zend/defaults/`, never overwriting.

**Admission by rating** (D-062, D-065): each file and memory is rated by its exact bytes; only 0–3 is
admitted (2–3 labelled with source and risk), 4–5 are named, never shown. A session depends on the
writers of what it admits, so denying a writer stops it. Stored content a risky session wrote keeps
its rating when handed over (`taint::hand_over_rating`).

**Fixed for the session.** Instructions and tools are written once (a `base` block, stored as an
`envelope`) and reused, so an edited AGENTS.md takes effect next session. An instruction file found
mid-session (a project below the workspace, `crates/zend/src/context.rs`) is attached once to the
tool result that touched it: as instructions if from a trusted repository (`ZEN_REPO`,
`ZEN_TRUSTED_REPOS`, or a worktree of one) and rated 0–3, else as rated content (D-051). It never
joins the instructions. Skills arrive as `load_skill` results; a `remember`ed memory shows from the
next session.

**Append-only history.** Nothing sent is rewritten. Tool output is cut once, when the tool runs,
head and tail (a shell's at 12 KB, a `read` page at 50 KB, other results at 64 KB), so the model sees
the same text every turn; the full capture is kept privately under `~/.zenbot/outputs/`.

## The tape

Each session is an append-only chain of blocks (`tape_events`): `seq` (its number in the session,
`#12`), `parent`, and `hash` over the parent's hash, kind and payload, so the last hash fingerprints
the session. Kinds: `message` (Pi's format; a user message may carry its turn `context`), `base`,
`context` (an instruction file picked up), `envelope`, `compaction`, `engine_session`, `failover`
(D-058), `risky`, `taint`, `allow`, `queue`, `trust` (D-054), and the agent tools' `questions`,
`verification`, `verdict`. Risk and origin are kernel columns beside the hashed payload.

## Summaries

`crates/zend/src/compact.rs`, rules from qm and goose. Past `ZEN_COMPACT_SOFT` (0.7) of the budget a
summary is prepared after the turn; it is applied at the next turn after `ZEN_COMPACT_IDLE_SECS`
(300, when the cache has expired anyway), or at once past `ZEN_COMPACT_HARD` (0.9). The budget is
`ZEN_CONTEXT_TOKENS` (200000) or the model's window if smaller; sizes are the provider's reported
context for the turn's last call, else the kernel's estimate.

- The latest turns (`ZEN_COMPACT_KEEP`, 0.3 of the budget, at least one turn) stay verbatim; the cut
  is at a turn's start, never between a call and its result. A summary is made only when it replaces
  at least half that much, since each one restarts the engine session.
- Fixed sections (goal, state, decisions, files, facts, open, next), every item citing blocks
  (`#156-#162`). Each summary builds on the previous one; if the summarizer fails, a plain one is
  built without a model.
- The summarizer reads the owner's words whole, other lines cut to 2,000 characters; over about
  400 KB, tool lines shrink first, then the agent's, then only their numbers remain.
- `ZEN_SUMMARY_MODEL` (`claude/claude-sonnet-5-5`) writes it, no tools. The `history` tool reads
  any block by number, so nothing is lost. Summaries and `history` results are marked as
  conversation data, not instructions.

## Engines

The kernel compiles the history; workers send it as their engine allows
([worker-protocol.md](worker-protocol.md), "Engine sessions"). Claude Code and Codex cache only
within their own sessions (measured: a resumed Claude Code session read 12,464 cached tokens, a
fresh one none; Codex 6,016 vs none), so each zenbot session keeps one engine session as a cache of
the tape, re-seeded from the tape after a summary, another engine's turn or an interrupted turn.
Codex gets history as native items. `ZEN_CLAUDE_RESUME`, `ZEN_CODEX_RESUME`, `ZEN_CODEX_INJECT`
turn these off.

## Measurement

Each `turns` row records `envelope` (fingerprint of instructions and tools), `context` (summary,
history range and size, turn context, `render`, estimate), `context_tokens` (provider-reported) and
`cache_break` (`crates/zend/src/measure.rs`): expected `first`, `instructions`, `summary`, `model`,
`engine_session`, `expired` (idle past the cache's life, `ZEN_CACHE_TTL`: Claude 1 h, else 5 min);
unexpected `history` (a bug) or `miss` (under half the previous context read from cache). A test
checks each turn's history starts with the previous turn's.
