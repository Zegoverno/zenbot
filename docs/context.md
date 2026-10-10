# Context

How zenbot builds, stores, measures and sends what a model reads on each turn. The model remembers
nothing between turns, so every turn zenbot sends the context again. The goals: as much of each
request as possible served from the provider's prompt cache, long sessions that never overflow,
nothing lost when older turns are summarized, every turn reproducible and measured.

Approaches are taken from the best of the projects studied (qm, goose, gbrain, caveman, eggshell,
memvid, Karpathy's LLM Wiki), checked in their code. Where none solved something, it says so.

## The request

Every request has the same order, from what never changes to what changes every turn. A provider
caches the beginning of a request that is identical to the previous one; the first difference ends
the discount, so anything that changes must come after everything that doesn't.

| Piece | Changes | Holds |
|---|---|---|
| Tools | per session | the kernel's tools, in a fixed order |
| Instructions (system prompt) | per session | the prompt files (`SOUL.md`, `IDENTITY.md`, `AGENTS.md`, `USER.md`), short-term memory (`MEMORY.md`), the skills index, the AGENTS.md files from `/` to the workspace |
| Summary | rarely | older turns, summarized, with the addresses of the originals |
| History | only appended to | messages since the summary, word for word |
| Turn context (footer) | per turn | the date when it changed; later, memories and search results |
| Prompt | per turn | what the owner typed |

- **What the instructions hold** (`compile::system_prompt`, D-027), in this order: `~/.zenbot/agents/zenbot/SOUL.md`
  (who the agent is), `~/.zenbot/agents/zenbot/IDENTITY.md` (how it works, D-045), `~/.zenbot/AGENTS.md` (its environment; `{{workspace}}`, `{{home}}`,
  `{{zen_home}}` and `{{repo}}` filled in), `~/.zenbot/USER.md` (the owner), short-term memory as it
  was at the session's start (entries `[m12] …`, plus a note on last night's sleep when there was
  one), the skills index (each skill's name and description, or only the domains when that's over
  `ZEN_SKILL_INDEX_CHARS`), the projects' instruction files, and last the tools' working directory.
  A scheduled job's session gets only the parts its `context` names (`compile::PARTS`; the soul is
  always in). Each prompt file has a size cap
  (`ZEN_SOUL_CHARS` 4000, `ZEN_IDENTITY_CHARS` 4000, `ZEN_AGENTS_CHARS` 12000, `ZEN_USER_CHARS` 3000); a longer file keeps its
  first 70% and last 20% with a note where to read the rest (OpenClaw's cut). The kernel writes
  default prompt files and skills that are missing when it starts (`crates/zend/defaults/`) and
  never overwrites one. How to use each tool is in the tool's description, not the instructions.
- **Loaded on demand, appended.** A skill (`load_skill`) arrives as a tool result, so the
  instructions never change mid-session; a memory saved with `remember` shows from the next session.
- **Instructions are fixed for the session.** They are written once on the first turn (a `base`
  block on the tape), stored with the tools (`envelopes`), and reused unchanged, so editing an AGENTS.md takes effect in the next session. An instruction file found
  mid-session (a project below the workspace) arrives once, attached to the tool result that touched
  it, and is never added to the instructions (goose adds it to the system prompt mid-session, which
  breaks the cache; qm keeps the system prompt byte-identical).
- **The date is not in the instructions.** It goes in the turn context, only when it changed
  (goose's turn-context message, qm's volatile footer, gbrain's `additionalContext`).
- **History is append-only.** Nothing already sent is rewritten. Tool output is cut once, when the
  tool runs (50 KB, head and tail), and the model sees the same text then and on every later turn;
  the full output is kept privately under `~/.zenbot/outputs/` and can be read back.

## The tape

Each session is an append-only chain of blocks in `tape_events`. Every block has:

- `seq`: its number within the session (`#12`), the address summaries and the `history` tool use.
  Numbers are per session, so parallel sessions never interleave.
- `parent`: the previous block. Today the chain is a line; the parent link is what branching and
  forking will use later (Pi's session tree).
- `hash`: sha256 of the parent's hash, the kind and the payload (as git does). The hash of the
  last block fingerprints the whole session up to it, and any change to stored history shows.

Kinds: `message` (user, assistant, toolResult, in Pi's message format; a user message may carry
`context`, the turn context sent with it), `base` (the session's instructions, written on its first
turn), `context` (an instruction file the session picked up), `envelope` (the session's instructions
and tools changed), `compaction` (a summary now replaces older blocks), `engine_session` (an
engine's own session is in sync with the tape up to a block), `failover` (a usage-limit switch to
another engine), `taint` (untrusted content entered the session), and the agent tools' records
`questions`, `verification`, `verdict` (docs/brief.md).

## Summaries

When a session's context passes 70% of the model's limit, a summary is prepared in the background
after the turn. It is applied at the start of the next turn after a pause (5 minutes, when the cache
has expired anyway), or at once when the context passes 90%. Applying it is one planned cache break;
every turn after it is smaller. Rules, from qm unless noted:

- The most recent turns (about 30% of the limit, at least the last turn) stay word for word. The
  cut is always at the start of a turn, so a tool call is never separated from its result. A summary
  is made only when what it would replace is at least half that size: each one restarts the engine's
  session, so it must remove a real chunk.
- The summary has fixed sections (goal, state, decisions, files, facts, open, next; goose's
  structured summary), and every item cites the blocks it came from (`#156-#162`).
- Each summary is built from the previous one plus the turns since. If the summarizer fails, a
  plain summary is built without a model (first request, later requests, files touched, last answer).
- The model has a `history` tool: read blocks by number or search the session. Nothing summarized
  is lost.
- The summarizer is `ZEN_SUMMARY_MODEL` (default `claude/claude-sonnet-5-5`), run through the
  owner's subscription with no tools.

The limit is a budget, not the model's window: long contexts cost more on every turn and models
read them worse, so a session is summarized when it passes 70% of `ZEN_CONTEXT_TOKENS` (default
200,000), or of the model's window if that is smaller. Sizes are the provider's reported context for
the turn's last model call, or the kernel's estimate for engines that report none.

Settings: `ZEN_CONTEXT_TOKENS` (default 200000),
`ZEN_COMPACT_SOFT` (0.7), `ZEN_COMPACT_HARD` (0.9), `ZEN_COMPACT_KEEP` (0.3),
`ZEN_COMPACT_IDLE_SECS` (300).

## Engines

The kernel compiles the history; each worker sends it the best way its engine allows. Where
delegating a step to the engine gives better results, the worker does it, behind the worker
protocol, with a fallback zenbot owns and a switch to turn it off.

- **Claude Code** caches earlier turns only inside its own sessions: measured, a resumed session read
  12,464 tokens from the cache where a fresh one read none, with a 1-hour cache lifetime. Claude
  Code also adds an environment block after the prompt and a separate call that names the session,
  both of which defeat caching for a fresh session per turn. So each zenbot session keeps a matching
  Claude Code session (`--session-id`, then `--resume` with only the new prompt), run from a fixed
  directory. The tape stays the source of truth and Claude Code's session is a cache: after a
  summary, a turn on another engine, or an interrupted turn, a new Claude Code session is seeded from
  the tape (the old quoted transcript, one cache miss). `ZEN_CLAUDE_RESUME=0` turns this off.
  No studied project does this; qm sends Claude Code a transcript every turn.
- **Codex** ties its prompt cache to the thread, so threads are kept the same way (`thread/resume`,
  measured: 6,016 cached tokens on a resumed turn, none on a new thread per turn). A new thread gets
  the history as native items (`thread/inject_items`, as qm does). `ZEN_CODEX_RESUME=0` and
  `ZEN_CODEX_INJECT=0` turn these off.

## Measurement

Every turn records in `turns`:

- `envelope`: the fingerprint of the instructions and tools sent.
- `context`: what was sent (summary used, history range and size, turn context, how it was sent:
  `resume`, `seed`, `inject`, `native`, `transcript`), and the size estimate.
- `context_tokens`: the context size the provider reported for the turn's last model call.
- `cache_break`: whether this request could not reuse the previous one's cache, and why: `first`,
  `instructions`, `summary`, `model`, `engine_session`, `history` (a bug), `expired` (a pause longer
  than the cache lives: 1 hour for Claude, 5 minutes otherwise), or `miss` (the provider read less
  than half the previous context from its cache). The eval report counts the unexpected ones.

A test checks that for a growing session each turn's history starts with the previous turn's
(goose's prefix-invariance test); the per-turn record does the same live (caveman's prefix monitor).

## Security

- Replayed history, summaries and `history` results are marked as conversation data, not
  instructions (gbrain, qm).
- Secrets are masked in tool output before the model or the tape sees it: the values of the
  kernel's own secrets and well-known token formats (API keys, GitHub and Slack tokens, private
  keys). A model that needs a secret's value should move it with the shell without printing it.
- Full outputs are stored under `~/.zenbot/outputs/`, readable only by the owner, not in `/tmp`.
- Summaries are written by an engine the owner already uses; no new service sees session data.
