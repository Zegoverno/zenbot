# Worker protocol

The kernel (`zend`) owns sessions, history and tools. A **worker** runs the model side of a turn. Any program that speaks this protocol can be a worker, so engines are swappable: zenbot ships `zen-engine` (Claude Code + Codex), and you can add your own.

System One typed decisions are not part of this protocol: `zend/src/score.rs` calls
OpenRouter directly (`POST /api/v1/systemone`, `ZEN_S1_URL` overrides it), mapping public `bool`
questions to `noul` on the wire. `ZEN_S1_MODEL` chooses the classifier (off when unset) and
`OPENROUTER_API_KEY` authenticates it.

## Transport

JSON-RPC 2.0 over the worker's stdin/stdout, one JSON object per line. The kernel starts each worker as a child process (see "Configuration"). Workers write logs to stderr.

The messages below are typed in `zen_proto::worker` (`crates/zen-proto/src/worker.rs`): `Request`
(kernel → worker), the answers, `ToolCall`/`ToolResult` and `TurnEvent` (the turn notifications),
with a round-trip test for each against samples shaped as below. `zend` and `zen-engine` build and
read them through these types; a worker in another language follows this page. Readers ignore
unknown fields and default missing or `null` ones; messages (below) and `turn.usage`'s report stay
open JSON. A change to the protocol changes this page, those types and their tests together.

## Kernel → worker

| Method | Params | Result |
|---|---|---|
| `ping` | `{}` | `{ "pong": true }` |
| `models.list` | `{}` | `{ "authenticated": { "<engine>": bool, … }, "models": [{ "id": "<engine>/<model>", "name": "…", "engine": "…", "efforts": ["low", …], "default_effort": "medium", "context"?: tokens }] }` |
| `turn.start` | `{ session_id, turn_id, model, effort, system_prompt, history, prompt, prompt_context, tools, resume, kind }` | `{ "ok": true }` immediately; the turn then runs asynchronously |
| `turn.abort` | `{ session_id, turn_id? }` | `{ "ok": true }`; the worker stops the turn (`turn_id`), or every turn it runs for the session, and sends `turn.end`. The kernel always names the turn; an abort for a turn the worker doesn't run yet must stop that turn when its `turn.start` comes (see "Abort") |
| `complete` | `{ model, system, prompt }` | `{ text, usage, model }` or `{ error }`: one completion without tools (the kernel uses it for summaries, session names and suggested next prompts; may take minutes) |

- `model` is one of the ids from `models.list`, always the model's full id (e.g. `claude/claude-opus-5-5`), never an alias that can move to another model. The kernel routes each model to the worker that listed it.
- `efforts` are the thinking levels a model accepts, in order, and `default_effort` the one used when a session picks none. Both are optional: a model without them has no level to choose. `context` (optional) is the model's context window in tokens; the kernel summarizes against the smaller of it and its own budget (docs/context.md).
- `effort` is the level for this turn: the session's choice, or the model's `default_effort`. The kernel always sends one for a model that has levels, so the level that ran is known; it is `null` only for models without levels. The worker must apply it, not substitute its own default.
- `system_prompt` and `tools` are fixed for the session (docs/context.md): send them as they are, so the provider's cache keeps hitting.
- `turn_id` is the kernel's id for this turn. The worker sends it back on every `tool.call` and notification of the turn (see "Late messages"). Messages without one are matched by session only, as from workers older than it.
- `history` is the session so far as the kernel compiled it, in the format below. Each message carries `seq`, its number on the tape (`#12`). A user message may carry `context`, the turn context that was sent after it; send it as a second text block after the message's content, as it was sent. When older turns were summarized, the first message is the summary: a user message with `"summary": true`. Don't drop or rewrite earlier messages: each turn's history starts with the previous turn's.
- `prompt_context` (string or null) is this turn's context (the date when it changed, …); send it as a text block after `prompt`.
- `kind` (string or null) is the kind of session: `verifier` for a child session the kernel runs to check work (the `verify` tool, docs/brief.md), `subagent` for a delegated task, `job` for a scheduled job's run; null for the owner's sessions.
- `resume` (`{ id }` or null) names an engine session of the worker's own that the kernel considers in sync with the tape up to this turn; the worker may continue it and send only the new prompt instead of the history (see "Engine sessions").
- `tools` is a list of `{ name, description, parameters }` with JSON Schema parameters. These are the only tools the model may use; the worker must not give the model tools of its own that touch the machine, and must check at run time that the engine has none: `zen-engine` refuses a Claude Code run whose `system/init` event doesn't come first or lists any tool but zenbot's, and stops a Codex turn in which Codex starts any item but a message, reasoning, a plan, a compaction or a zenbot dynamic tool call (its own shell, file changes, MCP, web search, …).

## Worker → kernel

| Message | Kind | Params |
|---|---|---|
| `tool.call` | request | `{ session_id, turn_id, call_id, name, args }` → result `{ content: string, is_error: bool }`. The kernel executes the tool. |
| `turn.delta` | notification | `{ session_id, turn_id, delta }`: streamed answer text |
| `turn.thinking` | notification | `{ session_id, turn_id, delta }`: streamed reasoning (optional) |
| `turn.message` | notification | `{ session_id, turn_id, message }`: a finished message, appended to the session's tape |
| `turn.usage` | notification | `{ session_id, turn_id, engine, engine_version, input?, output?, cache_read?, cache_write?, cost_usd?, render?, engine_session?, fallback_reason?, … }`: the worker's report for the whole turn, sent before `turn.end` |
| `turn.fallback` | notification | `{ session_id, turn_id, from, to, reason }`: a hard subscription limit caused one cross-provider continuation; the kernel records it and notifies clients |
| `turn.end` | notification | `{ session_id, turn_id, error }`: the turn is over; `error` is null on success, `"interrupted"` after an abort |

## Messages

Messages use these shapes (the same as Pi's message format):

```json
{ "role": "user", "content": "text", "timestamp": 1790000000000 }
{ "role": "assistant", "provider": "claude", "model": "claude-opus-5", "stopReason": "stop|toolUse|error",
  "content": [ { "type": "text", "text": "…" }, { "type": "thinking", "thinking": "…" },
               { "type": "toolCall", "id": "call-1", "name": "bash", "arguments": { "command": "ls" } } ],
  "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": { "total": 0.0 } }, "timestamp": 0 }
{ "role": "toolResult", "toolCallId": "call-1", "toolName": "bash", "content": [ { "type": "text", "text": "…" } ],
  "isError": false, "timestamp": 0 }
```

A worker emits one assistant message per model call, with that call's final usage and, when it can measure it, `durationMs`. It emits the assistant message announcing a tool call before calling `tool.call`, and a `toolResult` message after the kernel answers.

The tape must hold what the model read (DESIGN.md, invariant 2), so:

- A result the engine gave the model without the kernel (Claude Code refusing an unknown tool or invalid arguments) is sent as a `toolResult` message too.
- Every tool call on the tape gets a result before `turn.end`. A turn that ends while a call is still running waits a little for it (`zen-engine`: 10 s), then sends a `toolResult` with `isError: true` saying the call was interrupted and its effect is unknown; a real result that comes later is dropped.
- A tool call the worker can't match to the model's announcement (the engine's stream stopped announcing them) is logged and fails the turn, since its result can't be paired with a call.

`turn.usage` names the engine that ran the turn and its version (e.g. `claude-code` `2.1.287`), so an engine update shows up in the traces. Totals it gives (tokens, `cost_usd`) are taken for the turn as they are; for the ones it leaves out, the kernel sums the turn's messages. Give totals when the engine knows more than the messages show (Claude Code reports side calls only in its result; Codex reports tokens only per turn). Send `turn.usage` on every path, a failed start included.

A turn that ran on more than one provider (the quota continuation below) sends one report with `provider: "mixed"` and each provider's own report in `provider_reports`; the kernel makes each one per turn (see "Engine sessions") and adds them up.

## Engine sessions

Some engines cache earlier turns only inside their own sessions (measured for Claude Code and Codex, see docs/context.md). A worker may keep one engine session per kernel session, as a cache of the tape:

- It reports `engine_session: { id, resumable }` in `turn.usage`; `resumable` is true only when the turn finished cleanly, so the engine's session holds exactly what the tape holds.
- The kernel records it on the tape. On the next turn, if nothing was written since (no other turn, summary or new instructions) and the model is on the same engine, it sends `resume: { id }`.
- With `resume`, the worker continues that session and sends only the new prompt and its context. If the engine no longer has the session, or `resume` is null, it starts a new one from `history`.
- `fallback_reason` (string) says why the turn couldn't run as asked and fell back: the engine no longer had the session to `resume`, or refused the history as native items (so it went as a transcript). The worker also logs it.
- `render` says how the history reached the engine: `resume`, `seed` (new engine session from the history), `inject` (native items), `native` (messages), `transcript` (quoted text, no engine session).
- An engine that continues its own session may report totals for the whole session; the kernel makes them per turn by subtracting the previous turn's report for the same engine session.

Switches: `ZEN_CLAUDE_RESUME=0` and `ZEN_CODEX_RESUME=0` run every turn without an engine session; `ZEN_CODEX_INJECT=0` replays Codex history as a transcript.

`zen-engine` makes at most one cross-provider continuation after an **explicit hard subscription usage-limit** error (not a temporary throttle or generic 429): Claude → `codex/gpt-6.1-sol`, Codex → `claude/claude-sonnet-5-5`. It confirms both CLIs use subscription sign-in, refuses an API-key environment, includes the original prompt and every completed message/tool result in the new engine's history, and does not resume either engine session. If a tool call has no result or the captured transcript is too large, it stops with the original error rather than risking a duplicate action. `ZEN_QUOTA_FAILOVER=0` disables it; `ZEN_FAILOVER_CLAUDE_TO_CODEX` and `ZEN_FAILOVER_CODEX_TO_CLAUDE` override targets (full ids). The kernel records the switch in the tape and shows a status line. The final usage report keeps each provider's report in `provider_reports`; the kernel makes each one per turn (a resumed Claude session's totals are cumulative) and adds them up.

## What the kernel guarantees

- **Crashes.** The kernel restarts a worker that exits, with backoff. Turns it was running end with an error (`turn.end` is sent to clients by the kernel), and requests waiting on it fail at once.
- **Abort.** On `turn.abort` the kernel also stops its own tool calls for that session: a running command is killed (its whole process group), and the pending `tool.call` returns an `is_error` result saying it was interrupted. The kernel names the turn (`turn_id`), and if the turn was aborted while its `turn.start` was on the way, it sends `turn.abort` again once `turn.start` is answered. A worker must handle `turn.start` and `turn.abort` in the order they arrive, and remember turn ids aborted before their `turn.start` (`zen-engine` keeps the last 256) so such a turn ends at once with `"interrupted"`.
- **Stalled turns.** If a turn sends nothing and runs no tool for `ZEN_TURN_IDLE_SECS` (default 600), the kernel sends `turn.abort`. Any turn still running `ZEN_TURN_ABORT_GRACE_SECS` (default 30) after an abort is ended by the kernel.
- **Late messages.** Messages from a turn that has already ended (another `turn_id`, a session with no turn, or one running on another worker) are dropped; a late `tool.call` gets an `is_error` result. The kernel may end a turn the worker is still running (after an abort, see above), so the worker must key its running turns by `turn_id`, not by session: the session's next turn can start while the old one is still stopping.

So a worker never has to clean up after the kernel, but it must answer `turn.abort` promptly and always finish a turn with `turn.end`.

## Engines in `zen-engine`

`zen-engine` runs each model on an engine (`crates/zen-engine/src/engine.rs`): `claude`, `codex`
and, with `ZEN_FAUX=1`, `faux`. Each implements the `Engine` trait (its name, the model id prefix;
its CLI's version; its models; `run_turn`; `complete`; its failover target and subscription
sign-in check), and `engine::all()` lists them. The protocol loop and failover go through that
list, so adding an engine is one module implementing the trait and its line in `all`.

## Testing without a model

With `ZEN_FAUX=1`, `zen-engine` also lists `faux/smoke`, a scripted model that drives a real turn through the kernel: by default one `bash` call, then an answer. `ZEN_FAUX_SCRIPT` can point to a JSON list of steps (or an object of lists keyed by kind of session: `verify` for a verifier, `default` otherwise) (`{"tool": name, "args": {…}}`, `{"text": "…"}`, `{"sleep": secs}`, `{"exit": code}`; a step with `"when": "<text>"` runs only in turns whose prompt contains the text, and one with `"ignore_abort": true` keeps running through an abort) to test tools, abort, the watchdog, late messages and crash recovery. `scripts/upgrade.sh` runs one such turn against the new build before installing it.

## Configuration

`ZEN_WORKERS` lists the workers to start (comma-separated, default `engine`):

| Name | Command | What it serves |
|---|---|---|
| `engine` | `zen-engine` next to `zend` (override with `ZEN_ENGINE_CMD`) | `claude/*` via the Claude Code CLI, `codex/*` via `codex app-server`, on the owner's subscriptions; keeps engine sessions (see above) |
| any other `name` | `ZEN_WORKER_<NAME>_CMD` | whatever its `models.list` returns |

A name without its `ZEN_WORKER_<NAME>_CMD` (a retired worker such as `pi`, a typo) is ignored: the kernel logs a warning and reports it in `/health` (`ignored_workers`) and `zen status`, and runs without it. If no name is left, it starts `engine`.

The kernel starts each worker with `bash -c <command>` (not a login shell), without its own secrets: every variable named like a secret (`secrets::is_secret_var`: its token, `DATABASE_URL`, the OpenRouter and web search keys, …) is removed, except the engines' sign-ins (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `OPENAI_API_KEY`, `CODEX_API_KEY`, `CODEX_ACCESS_TOKEN`).

If a worker doesn't answer `models.list` when the kernel refreshes its models, the kernel keeps routing the models it listed before. `zen-engine` asks Codex for its models with a 15 s limit and keeps the list for an hour, or until the `codex` binary changes; a failed lookup returns the previous list.
