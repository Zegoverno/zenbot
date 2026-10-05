# Client protocol

How a client (the `zen` terminal app and CLI, the web UI, scripts) talks to the kernel. Workers use
a different protocol (docs/worker-protocol.md).

Every request carries the owner's token: `Authorization: Bearer <token>` (or `?token=` for the web
UI). The token is in `~/.zenbot/token`.

## HTTP

| Method and path | Body | Returns |
|---|---|---|
| `GET /health` (no token) | | `{ ok, db, mind, workers, busy, version, commit }`; `busy` counts running turns and workflow steps |
| `GET /api/models` | | `{ models, authenticated, default, scorer, classifiers, workers }` |
| `GET /api/sessions?archived=` | | sessions (not child sessions): `{ id, title, model, effort, archived, state, cost, created_at, updated_at }` |
| `POST /api/sessions` | `{ title?, model?, effort? }` | the session |
| `GET /api/sessions/{id}` | | the session with `messages` (each with its `seq`) and `busy` |
| `PATCH /api/sessions/{id}` | `{ title?, model?, effort? ("default" clears), archived? }` | the session |
| `POST /api/sessions/{id}/decision` | `{ decision: accept\|more\|reshape\|drop, note? }` | the recorded verdict (source `owner`) |
| `POST /api/sessions/{id}/flow` | `{ action: brief\|quick\|go\|verify }` | `{ state }`; 409 when the action doesn't apply now |
| `GET /api/version?refresh=` | | the running commit and whether `main` is ahead |
| `GET /api/upgrade`, `POST /api/upgrade` | | upgrade progress / start one |

## WebSocket: `/api/sessions/{id}/ws`

The client sends:

| Message | Effect |
|---|---|
| `{ "type": "prompt", "text": "…" }` | the owner's message: starts a turn, or approves a waiting brief ("yes", "go"), or continues the job after a report (docs/brief.md) |
| `{ "type": "abort" }` | stops the running turn |

The kernel sends events, in order:

| Event | Fields | Meaning |
|---|---|---|
| `message` | `message` | a message added to the session (Pi's format, with `seq`). A user message with `kernel: true` is the workflow talking to the model, not the owner |
| `busy` | `turn_id, harness, model, effort` | a turn started (also turns the kernel started itself) |
| `delta`, `thinking` | `delta` | streamed answer text / reasoning |
| `tool_start` | `call_id, name, args` | a tool call began |
| `tool_end` | `call_id, is_error, ms` | it finished |
| `end` | `error, cost, turn, next` | the turn ended. `turn` is the kernel's record (tokens, cost, time, `cache_break`, `context_tokens`, `render`). With `next: true` the workflow continues on its own: keep waiting for `idle` |
| `child_end` | `turn` | a child session's turn ended (a verifier); count its cost with the job |
| `idle` | `state, waiting?` | the workflow stopped and waits for the owner: `waiting` is `approval` or `answers`, or the session is `reported` / `closed` |
| `state` | `state, by, reason` | the session moved to another state (`framing`, `working`, `verifying`, `reported`, `closed`, `open`) |
| `status` | `text` | progress of a workflow step ("verifying: running 3 checks") |
| `brief` | `version, brief, text` | a proposed brief; `text` is rendered for reading |
| `questions` | `questions: [{ question, options }]` | questions for the owner, recommended option first |
| `report` | `text, results` | the verification report |
| `error` | `error` | a request failed (e.g. prompting while the work is being verified) |
| `resync` | `skipped, busy` | the client fell behind and missed events |
| `usage` | `input, output, cost` | (older kernels) usage reported outside messages |

A client that predates an event can ignore it, except `end.next` and `idle`: a client that stops at
the first `end` misses the rest of a workflow (approval, work, verification).
