# Worker protocol

The kernel (`zend`) owns sessions, history and tools. A **worker** runs the model side of a turn. Any program that speaks this protocol can be a worker, so engines are swappable: zenbot ships `zen-engine` (Claude Code + Codex) and `zen-mind` (Pi), and you can add your own.

## Transport

JSON-RPC 2.0 over the worker's stdin/stdout, one JSON object per line. The kernel starts each worker as a child process (see "Configuration"). Workers write logs to stderr.

## Kernel → worker

| Method | Params | Result |
|---|---|---|
| `ping` | `{}` | `{ "pong": true }` |
| `models.list` | `{}` | `{ "authenticated": { "<engine>": bool, … }, "models": [{ "id": "<engine>/<model>", "name": "…", "efforts": ["low", …], "default_effort": "medium" }], "classifiers": [{ "id": "<provider>/<model>", "name": "…" }] }` |
| `turn.start` | `{ session_id, model, effort, system_prompt, history, prompt, prompt_context, tools, resume }` | `{ "ok": true }` immediately; the turn then runs asynchronously |
| `turn.abort` | `{ session_id }` | `{ "ok": true }`; the worker stops the turn and sends `turn.end` |
| `complete` | `{ model, system, prompt }` | `{ text, usage, model }` or `{ error }`: one completion without tools (the kernel uses it for summaries; may take minutes) |
| `s1.decide` | `{ model, state, questions }` | `{ model, provider, answers, usage, error }` (optional; only workers that list `classifiers`) |

- `model` is one of the ids from `models.list`, always the model's full id (e.g. `claude/claude-opus-5-5`), never an alias that can move to another model. The kernel routes each model to the worker that listed it.
- `efforts` are the thinking levels a model accepts, in order, and `default_effort` the one used when a session picks none. Both are optional: a model without them has no level to choose.
- `effort` is the level for this turn: the session's choice, or the model's `default_effort`. The kernel always sends one for a model that has levels, so the level that ran is known; it is `null` only for models without levels. The worker must apply it, not substitute its own default.
- `system_prompt` and `tools` are fixed for the session (docs/context.md): send them as they are, so the provider's cache keeps hitting.
- `history` is the session so far as the kernel compiled it, in the format below. Each message carries `seq`, its number on the tape (`#12`). A user message may carry `context`, the turn context that was sent after it; send it as a second text block after the message's content, as it was sent. When older turns were summarized, the first message is the summary: a user message with `"summary": true`. Don't drop or rewrite earlier messages: each turn's history starts with the previous turn's.
- `prompt_context` (string or null) is this turn's context (the date when it changed, …); send it as a text block after `prompt`.
- `resume` (`{ id }` or null) names an engine session of the worker's own that the kernel considers in sync with the tape up to this turn; the worker may continue it and send only the new prompt instead of the history (see "Engine sessions").
- `tools` is a list of `{ name, description, parameters }` with JSON Schema parameters. These are the only tools the model may use; the worker must not give the model tools of its own that touch the machine.
- `classifiers` are System One models (typed decisions, e.g. TypeSafe's Jev) the worker can run with `s1.decide`; the kernel uses one for live session scoring when `ZEN_S1_MODEL` names it. `state` is a JSON object; `questions` maps a key to `{ type: "choice", instructions, criteria: { option: description } }`, `{ type: "score", instructions, criteria: [level, …] }` (low to high) or `{ type: "bool", instructions, criteria: { true, false } }`. Answers are `{ type: "choice", choice, probabilities, confidence }`, `{ type: "score", score, confidence }` or `{ type: "bool", probability }`. A failed call returns `error` instead of failing the request. The Pi worker serves OpenRouter's System One models (with `OPENROUTER_API_KEY`).

## Worker → kernel

| Message | Kind | Params |
|---|---|---|
| `tool.call` | request | `{ session_id, call_id, name, args }` → result `{ content: string, is_error: bool }`. The kernel executes the tool. |
| `turn.delta` | notification | `{ session_id, delta }`: streamed answer text |
| `turn.thinking` | notification | `{ session_id, delta }`: streamed reasoning (optional) |
| `turn.message` | notification | `{ session_id, message }`: a finished message, appended to the session's tape |
| `turn.usage` | notification | `{ session_id, engine, engine_version, input?, output?, cache_read?, cache_write?, cost_usd?, render?, engine_session?, … }`: the worker's report for the whole turn, sent before `turn.end` |
| `turn.end` | notification | `{ session_id, error }`: the turn is over; `error` is null on success, `"interrupted"` after an abort |

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

`turn.usage` names the engine that ran the turn and its version (e.g. `claude-code` `2.1.287`), so an engine update shows up in the traces. Totals it gives (tokens, `cost_usd`) are taken for the turn as they are; for the ones it leaves out, the kernel sums the turn's messages. Give totals when the engine knows more than the messages show (Claude Code reports side calls only in its result).

## Engine sessions

Some engines cache earlier turns only inside their own sessions (measured for Claude Code and Codex, see docs/context.md). A worker may keep one engine session per kernel session, as a cache of the tape:

- It reports `engine_session: { id, resumable }` in `turn.usage`; `resumable` is true only when the turn finished cleanly, so the engine's session holds exactly what the tape holds.
- The kernel records it on the tape. On the next turn, if nothing was written since (no other turn, summary or new instructions) and the model is on the same engine, it sends `resume: { id }`.
- With `resume`, the worker continues that session and sends only the new prompt and its context. If the engine no longer has the session, or `resume` is null, it starts a new one from `history`.
- `render` says how the history reached the engine: `resume`, `seed` (new engine session from the history), `inject` (native items), `native` (messages), `transcript` (quoted text, no engine session).
- An engine that continues its own session may report totals for the whole session; the kernel makes them per turn by subtracting the previous turn's report for the same engine session.

Switches: `ZEN_CLAUDE_RESUME=0` and `ZEN_CODEX_RESUME=0` run every turn without an engine session; `ZEN_CODEX_INJECT=0` replays Codex history as a transcript.

## What the kernel guarantees

- **Crashes.** The kernel restarts a worker that exits, with backoff. Turns it was running end with an error (`turn.end` is sent to clients by the kernel), and requests waiting on it fail at once.
- **Abort.** On `turn.abort` the kernel also stops its own tool calls for that session: a running command is killed (its whole process group), and the pending `tool.call` returns an `is_error` result saying it was interrupted.
- **Stalled turns.** If a turn sends nothing and runs no tool for `ZEN_TURN_IDLE_SECS` (default 600), the kernel sends `turn.abort`. Any turn still running 30 seconds after an abort is ended by the kernel.
- **Late messages.** Messages for a session whose turn has already ended (or runs on another worker) are dropped; a late `tool.call` gets an `is_error` result.

So a worker never has to clean up after the kernel, but it must answer `turn.abort` promptly and always finish a turn with `turn.end`.

## Testing without a model

With `ZEN_FAUX=1`, `zen-engine` also lists `faux/smoke`, a scripted model that drives a real turn through the kernel: by default one `bash` call, then an answer. `ZEN_FAUX_SCRIPT` can point to a JSON list of steps (or an object of lists keyed by workflow phase, `frame`, `work`, `verify`, `default`, to drive a whole briefed session) (`{"tool": name, "args": {…}}`, `{"text": "…"}`, `{"sleep": secs}`, `{"exit": code}`) to test tools, abort, the watchdog and crash recovery. `scripts/upgrade.sh` runs one such turn against the new build before installing it.

## Configuration

`ZEN_WORKERS` lists the workers to start (comma-separated, default `engine`):

| Name | Command | What it serves |
|---|---|---|
| `engine` | `zen-engine` next to `zend` (override with `ZEN_ENGINE_CMD`) | `claude/*` via the Claude Code CLI, `codex/*` via `codex app-server`, on the owner's subscriptions; keeps engine sessions (see above) |
| `pi` | `node src/main.ts` in `ZEN_MIND_DIR` (override with `ZEN_MIND_CMD`) | `openai/*` via Pi's direct ChatGPT sign-in, and Pi's API providers |
| any other `name` | `ZEN_WORKER_<NAME>_CMD` | whatever its `models.list` returns |
