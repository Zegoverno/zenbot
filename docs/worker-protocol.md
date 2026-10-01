# Worker protocol

The kernel (`zend`) owns sessions, history and tools. A **worker** runs the model side of a turn. Any program that speaks this protocol can be a worker, so engines are swappable: zenbot ships `zen-engine` (Claude Code + Codex) and `zen-mind` (Pi), and you can add your own.

## Transport

JSON-RPC 2.0 over the worker's stdin/stdout, one JSON object per line. The kernel starts each worker as a child process (see "Configuration"). Workers write logs to stderr.

## Kernel → worker

| Method | Params | Result |
|---|---|---|
| `ping` | `{}` | `{ "pong": true }` |
| `models.list` | `{}` | `{ "authenticated": { "<engine>": bool, … }, "models": [{ "id": "<engine>/<model>", "name": "…" }] }` |
| `turn.start` | `{ session_id, model, system_prompt, history, prompt, tools }` | `{ "ok": true }` immediately; the turn then runs asynchronously |
| `turn.abort` | `{ session_id }` | `{ "ok": true }`; the worker stops the turn and sends `turn.end` |

- `model` is one of the ids from `models.list`. The kernel routes each model to the worker that listed it.
- `history` is the session so far (already trimmed by the kernel), as messages in the format below.
- `tools` is a list of `{ name, description, parameters }` with JSON Schema parameters. These are the only tools the model may use; the worker must not give the model tools of its own that touch the machine.

## Worker → kernel

| Message | Kind | Params |
|---|---|---|
| `tool.call` | request | `{ session_id, call_id, name, args }` → result `{ content: string, is_error: bool }`. The kernel executes the tool. |
| `turn.delta` | notification | `{ session_id, delta }`: streamed answer text |
| `turn.thinking` | notification | `{ session_id, delta }`: streamed reasoning (optional) |
| `turn.message` | notification | `{ session_id, message }`: a finished message, appended to the session's tape |
| `turn.usage` | notification | `{ session_id, provider, model, input, output, cache_read, cache_write, cost_usd }`: usage reported per turn (optional) |
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

A worker emits the assistant message announcing a tool call before calling `tool.call`, and a `toolResult` message after the kernel answers.

## Configuration

`ZEN_WORKERS` lists the workers to start (comma-separated, default `engine`):

| Name | Command | What it serves |
|---|---|---|
| `engine` | `zen-engine` next to `zend` (override with `ZEN_ENGINE_CMD`) | `claude/*` via the Claude Code CLI, `codex/*` via `codex app-server`, on the owner's subscriptions |
| `pi` | `node src/main.ts` in `ZEN_MIND_DIR` (override with `ZEN_MIND_CMD`) | `openai/*` via Pi's direct ChatGPT sign-in, and Pi's API providers |
| any other `name` | `ZEN_WORKER_<NAME>_CMD` | whatever its `models.list` returns |
