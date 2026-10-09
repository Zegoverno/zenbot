# Client protocol

How a client (the `zen` terminal app and CLI, the web UI, scripts) talks to the kernel. Workers use
a different protocol (docs/worker-protocol.md).

Every request carries the owner's token in `Authorization: Bearer <token>`. Only a WebSocket
upgrade accepts `?token=` (the browser WebSocket API cannot set that header); URL tokens are not
accepted for ordinary HTTP requests. The token is in `~/.zenbot/token`. The server listens on
`127.0.0.1:8100` by default; `ZEN_BIND` and `ZEN_PORT` change the address and port.

## HTTP

| Method and path | Body | Returns |
|---|---|---|
| `GET /health` (no token) | | `{ ok, db, mind, workers, busy, version, commit }`; `busy` counts running turns and kernel work outside them |
| `GET /api/models` | | `{ models, authenticated, default, scorer }` |
| `GET /api/sessions?archived=` | | sessions (not child sessions): `{ id, title, model, effort, archived, cost, created_at, updated_at }` |
| `POST /api/sessions` | `{ title?, model?, effort? }` | the session |
| `GET /api/sessions/{id}?last=&after=` | | the session with `messages` (each with its `seq`; all of them, or the `last` N, or those after block `after`) and `busy` |
| `PATCH /api/sessions/{id}` | `{ title?, model?, effort? ("default" clears), archived? }` | the session. A title set here is the owner's: the kernel no longer renames the session |
| `GET /api/suggestions` | | suggested next prompts: outcomes by prompt version and model, and the latest 20 |
| `POST /api/sessions/{id}/decision` | `{ decision: accept\|more\|reshape\|drop, note? }` | the recorded verdict (source `owner`) |
| `GET /api/memory?tier=` | | `{ memories, last_sleep, size }`: memories of a tier (`short`, the default; `archived`, `all`; `long` holds only rows from before D-045), each `{ id: "m12", text, source, tier, proposed, reason, created_at, updated_at }`; the latest `sleep_runs` row; short-term memory's size in characters |
| `POST /api/memory/sleep?trigger=` | | tidy short-term memory now (`trigger=nightly` from the timer, else the owner): `{ run, entries, kept, dropped, promoted, scorer, note }` |
| `GET /api/version?refresh=` | | the running commit and whether `main` is ahead |
| `GET /api/upgrade`, `POST /api/upgrade` | | upgrade progress / start one |

## WebSocket: `/api/sessions/{id}/ws`

The client sends:

| Message | Effect |
|---|---|
| `{ "type": "prompt", "text": "…", "suggestion"?: { "id", "taken" } }` | the owner's message: starts a turn. `suggestion` reports the suggested next prompt the client showed: `taken` when the owner took it into the input (tab); the kernel records `accepted` (sent unchanged), `edited` or `declined`. A suggestion the client didn't report is recorded `unseen` |
| `{ "type": "abort" }` | stops the running turn |

The kernel sends events, in order:

| Event | Fields | Meaning |
|---|---|---|
| `message` | `message` | a message added to the session (Pi's format, with `seq`). A user message with `kernel: true` is the kernel talking to the model (a verifier's instructions), not the owner |
| `busy` | `turn_id, harness, model, effort` | a turn started (also turns the kernel started itself) |
| `delta`, `thinking` | `delta` | streamed answer text / reasoning |
| `tool_start` | `call_id, name, args` | a tool call began |
| `tool_end` | `call_id, is_error, ms` | it finished |
| `end` | `error, cost, turn, next` | the turn ended. `turn` is the kernel's record (tokens, cost, time, `cache_break`, `context_tokens`, `render`). With `next: true` the kernel starts another turn itself: keep waiting for `idle` (nothing sends it today) |
| `child_end` | `turn` | a child session's turn ended (a verifier); count its cost with the job |
| `idle` | | the kernel's own turns are over (after `end` with `next: true`) |
| `title` | `title` | the kernel named the session (after one of its first turns, unless the owner set the title) |
| `suggestion` | `id, text` | after an owner's turn: the owner's likely next prompt. Show it until the next prompt is sent (zen: grey in the empty input, tab takes it); send its `id` back with that prompt |
| `status` | `text` | progress of a long tool ("verifying: running 3 check(s)") |
| `questions` | `questions: [{ question, options }]` | the `ask` tool's questions for the owner, recommended option first; the answers are the owner's next prompt |
| `error` | `error` | a request failed |
| `resync` | `skipped, busy` | the client fell behind and missed events |

A client that predates an event can ignore it. The old workflow's events (`brief`, `report`,
`state`, `idle` with `waiting`) and `POST /api/sessions/{id}/flow` were removed on 2026-10-07.
