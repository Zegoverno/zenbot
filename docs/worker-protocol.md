# Worker protocol

The kernel (`zend`) owns sessions, history and tools; a **worker** runs the model side of a turn.
zenbot ships `zen-engine` (Claude Code + Codex). System One isn't part of this protocol
(`crates/zend/src/s1/wire.rs` calls OpenRouter).

## Transport

JSON-RPC 2.0 over stdin/stdout, one object per line; logs to stderr. Typed in
`crates/zen-proto/src/worker.rs` (`Request`, answers, `ToolCall`/`ToolResult`, `TurnEvent`, each
round-trip tested). Readers ignore unknown fields and default missing or `null` ones; messages and
the usage report stay open JSON. A change updates this page, those types and every worker.

## Kernel → worker

| Method | Params | Result |
|---|---|---|
| `ping` | `{}` | `{ "pong": true }` |
| `models.list` | `{}` | `{ "authenticated": { "<engine>": bool, … }, "models": [{ "id": "<engine>/<model>", "name", "engine", "efforts"?: ["low", …], "default_effort"?, "context"?: tokens, … }] }` |
| `turn.start` | `{ session_id, turn_id, model, effort, system_prompt, history, prompt, prompt_context, tools, resume, kind }` | `{ "ok": true }` at once; the turn then runs asynchronously |
| `turn.abort` | `{ session_id, turn_id? }` | `{ "ok": true }`; stop that turn (or every turn of the session) and send `turn.end` |
| `complete` | `{ model, system, prompt }` | `{ text, usage, model }` or `{ error }`: one completion without tools (summaries, session names, suggestions; may take minutes) |

- `authenticated`: per engine, installed and answering (sign-in is checked per turn). Extra model
  fields (Codex's `default`) pass through.
- `model`: always a full id from `models.list`, never a moving alias; the kernel routes it to the
  worker that listed it.
- `efforts` (in order), `default_effort` (when the session picks none), `context` (window in tokens;
  the kernel summarizes against the smaller of it and its budget): all optional.
- `effort`: the session's level or the model's default; `null` only for a model without levels. The
  worker applies it, never its own default.
- `system_prompt`, `tools`: fixed for the session; send unchanged so the provider cache hits.
- `turn_id`: sent back on every `tool.call` and notification of the turn (without one, matched by
  session only).
- `history`: the compiled session, messages below, each with its tape `seq`. A user message's
  `context` (its turn context) goes as a second text block after its content. After a summary, the
  first message is a user message with `"summary": true`. Never drop or rewrite messages: each
  turn's history starts with the previous turn's.
- `prompt_context` (string or null): this turn's context (the date when it changed), a text block
  after `prompt`.
- `kind` (string or null): `verifier` (a child session checking work, the `verify` tool),
  `subagent` (a delegated task), `job` (a scheduled job's run); null for the owner's sessions.
- `resume` (`{ id }` or null): an engine session of the worker's own that is in sync with the tape
  up to this turn ("Engine sessions").
- `tools`: `{ name, description, parameters }` (JSON Schema), the only tools the model may use. The
  worker checks at run time that the engine has none of its own: `zen-engine` refuses a Claude Code
  run whose `system/init` isn't first or lists another tool, and stops a Codex turn that starts any
  item but a message, reasoning, a plan, a compaction or a zenbot dynamic tool call.

## Worker → kernel

| Message | Kind | Params |
|---|---|---|
| `tool.call` | request | `{ session_id, turn_id, call_id, name, args }` → `{ content: string, is_error: bool, content_blocks?: [...] }`. The kernel runs it. `content` has source labels; optional MCP-shaped text/image `content_blocks` go in the tape message after the text, mapped to the provider |
| `turn.delta` | notification | `{ session_id, turn_id, delta }`: streamed answer text |
| `turn.thinking` | notification | `{ session_id, turn_id, delta }`: streamed reasoning (optional) |
| `turn.message` | notification | `{ session_id, turn_id, message }`: a finished message, appended to the tape |
| `turn.usage` | notification | `{ session_id, turn_id, engine, engine_version, provider?, model?, input?, output?, cache_read?, cache_write?, cost_usd?, totals?, render?, engine_session?, fallback_reason?, provider_reports?, … }`: the report for the whole turn, before `turn.end` |
| `turn.fallback` | notification | `{ session_id, turn_id, from, to, reason }`: a hard subscription limit caused one cross-provider continuation |
| `turn.end` | notification | `{ session_id, turn_id, error }`: the turn is over; `error` null on success, `"interrupted"` after an abort |

## Messages

Pi's message format:

```json
{ "role": "user", "content": "text", "timestamp": 1790000000000 }
{ "role": "assistant", "provider": "claude", "model": "claude-opus-5", "stopReason": "stop|toolUse|error",
  "content": [ { "type": "text", "text": "…" }, { "type": "thinking", "thinking": "…" },
               { "type": "toolCall", "id": "call-1", "name": "bash", "arguments": { "command": "ls" } } ],
  "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": { "total": 0.0 } }, "timestamp": 0 }
{ "role": "toolResult", "toolCallId": "call-1", "toolName": "bash", "content": [ { "type": "text", "text": "…" } ],
  "isError": false, "timestamp": 0 }
```

One assistant message per model call, with its final usage and, when known, `durationMs`; the
message announcing a tool call precedes its `tool.call`, a `toolResult` follows the answer. The tape
holds what the model read: a result the engine gave without the kernel (Claude Code refusing a bad
call) is sent too; every call gets a result before `turn.end` (one still running is waited for,
`zen-engine` 10 s, then gets an `isError` "interrupted, effect unknown" result; a later real one is
dropped); a call that can't be matched to its announcement is logged and fails the turn.

## Usage

`engine`, `engine_version` name what ran. Totals given (tokens, `cost_usd`) are taken as is; the
kernel sums the turn's messages for the rest (give totals when the engine knows more, e.g. Claude
Code side calls). `input` is uncached only; cached is `cache_read` (D-075). A resumed engine
session's totals count as cumulative (the kernel subtracts the previous turn's) unless `totals:
"turn"`. Without `cost_usd`, `crates/zend/src/prices.rs` estimates one. Send it on every path, a
failed start included. After a failover: `provider: "mixed"` and each report in `provider_reports`.

## Engine sessions

Engines cache earlier turns only inside their own sessions, so a worker may keep one per kernel
session as a cache of the tape. It reports `engine_session: { id, resumable }` (`resumable` only
after a clean finish); if nothing was written since and the model is on the same engine, the next
turn gets `resume: { id }` and the worker sends only the new prompt and context; otherwise (or if
the engine lost it) it starts from `history`. `fallback_reason` says why a turn couldn't run as
asked; `render` how history reached the engine: `resume`, `seed`, `inject` (native items), `native`,
`transcript`.

`ZEN_CLAUDE_RESUME=0` and `ZEN_CODEX_RESUME=0` run every turn without an engine session;
`ZEN_CODEX_INJECT=0` replays Codex history as a transcript.

**Failover.** After an explicit hard subscription usage limit (not a throttle), `zen-engine` makes
one cross-provider continuation: Claude → `codex/gpt-6.1-sol`, Codex → `claude/claude-sonnet-5-5`
(`ZEN_FAILOVER_CLAUDE_TO_CODEX`, `ZEN_FAILOVER_CODEX_TO_CLAUDE`; `ZEN_QUOTA_FAILOVER=0` off). Both
CLIs must use subscription sign-in. The new engine gets the prompt and every completed message, no
session is resumed; an unanswered tool call or an oversized transcript keeps the original error. The
kernel records it, and the session keeps the target model until the owner picks another (D-058).

## What the kernel guarantees

- **Crashes.** A worker that exits restarts with backoff; its turns end with an error.
- **Abort.** The kernel kills the session's running command (process group) and answers the pending
  `tool.call` "interrupted". An abort sent while `turn.start` was in flight is sent again once it is
  answered; workers handle both in arrival order and remember ids aborted before their start
  (`zen-engine`: 256), ending such a turn at once with `"interrupted"`.
- **Stalled turns.** Silent for `ZEN_TURN_IDLE_SECS` (600): `turn.abort`; still running
  `ZEN_TURN_ABORT_GRACE_SECS` (30) later: ended by the kernel.
- **Late messages** for an ended turn are dropped (a late `tool.call` gets `is_error`). Key turns
  by `turn_id`: the next can start while the old one stops. Always end a turn with `turn.end`.

## `zen-engine`

Engines (`claude`, `codex`, `faux` with `ZEN_FAUX=1`) implement the `Engine` trait in
`crates/zen-engine/src/engine.rs`; `engine::all()` lists them. Codex's models are asked with a 15 s
limit and cached an hour (or until `codex` changes).

**Without a model.** `faux/smoke` (`ZEN_FAUX=1`) scripts real turns: by default a `bash` call, then
an answer. `ZEN_FAUX_SCRIPT`: a JSON list of steps (or lists keyed
`verify`/`default`): `{"tool", "args"}`, `{"text"}`, `{"sleep": secs}`, `{"exit": code}`, optionally
`"when": "<prompt text>"`, `"ignore_abort": true`. `scripts/upgrade.sh` smoke-tests builds this way.

## Configuration

`ZEN_WORKERS` lists the workers to start (comma-separated, default `engine`):

| Name | Command | Serves |
|---|---|---|
| `engine` | `zen-engine` next to `zend` (`ZEN_ENGINE_CMD` overrides) | `claude/*` via the Claude Code CLI, `codex/*` via `codex app-server`, on the owner's subscriptions |
| any other | `ZEN_WORKER_<NAME>_CMD` | whatever its `models.list` returns |

An unknown name (e.g. retired `pi`) is ignored and shown in `/health` (`ignored_workers`) and
`zen status`. Each worker runs under `bash -c <command>` (not a login shell) without the kernel's secrets: every
secret-named variable (`secrets::is_secret_var`) is removed except the engines' sign-ins
(`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `OPENAI_API_KEY`,
`CODEX_API_KEY`, `CODEX_ACCESS_TOKEN`). A worker that misses a `models.list` refresh keeps its models.
