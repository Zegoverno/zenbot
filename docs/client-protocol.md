# Client protocol

How clients (`zen`, `zen-matrix`, scripts) talk to the kernel; workers use
[worker-protocol.md](worker-protocol.md). Every request, the WebSocket upgrade included, carries
`Authorization: Bearer <token>` (`~/.zenbot/token`; never in the URL) on
`127.0.0.1:8100` (`ZEN_BIND`, `ZEN_PORT`). `crates/zen-client` makes these calls for Rust clients.

## HTTP

Paths under `/api` but `/health`. Owner decisions (allow, trust, queue, reviews) are never model tools.

| Route | Body | Returns |
|---|---|---|
| `GET /health` (no token) | | `{ ok, db, mind, workers, ignored_workers, busy, version, commit }` |
| `GET /models` | | `{ models, authenticated, default, scorer }` |
| `GET /sessions?archived=` | | owner and job sessions `{ id, title, model, effort, archived, kind, parent, cost, created_at, updated_at }` |
| `GET /board?archived=` | | `{ sessions, archived }`: 200 top-level sessions, their latest 20 and running children, `busy`, `waiting`, `children`/`task` |
| `POST /sessions` | `{ title?, model?, effort? }` | the session |
| `GET /sessions/{id}?last=&after=` | | the session with `messages` (each with `seq`), `message_count`, `tokens`, `busy` |
| `PATCH /sessions/{id}` | `{ title?, model?, effort? ("default" clears), archived? }` | the session; a title set here stops auto-naming |
| `GET /sessions/{id}/taint` | | `{ risky_at, tainted_at, taints, sources, checked, busy }` |
| `POST /sessions/{id}/trust` | `{ seen }` (`checked` above) | `{ trusted: true, seq }` or `{ trusted: false, result }`; 409 while busy or if it read more after `seen` (D-054) |
| `POST /sessions/{id}/deny` | | `{ stopped: true }`: stops the session, marks its products contaminated |
| `GET`/`POST /sessions/{id}/allow` | `{ id }` | held actions `{ actions: [{ id, action, reason }], busy }` / `{ allowed }`: that exact action once, within ten minutes; refused during a turn |
| `POST /sessions/{id}/decision` | `{ decision: accept\|more\|reshape\|drop, note? }` | `{ id, session_id, turn_id, decision, note, created_at }` |
| `GET /queue` | | pending held actions and source proposals: `{ kind: action\|source, id, summary, reason, risk, created_at, … }` |
| `POST /queue/{kind}/{id}` | `{ decision: approve\|reject\|deny }` | the result; compare-and-set (409 if stale or busy); `deny` only for actions |
| `GET`/`POST /sources` | `{ source, risk?: 2\|3, note?, remove? }` | the source allowlist `[{ source, risk, note }]` / the change |
| `POST /sources/rerate` | | re-rates them |
| `GET /trust?suggest=`, `POST /trust` | `{ kind, match, level?, note?, remove? }` | `~/.zenbot/trust.toml` entries (with `suggest`: hosts seen clean, for review) / the change |
| `GET /s1`, `/s1/check`, `/s1/models` | | System One stats (30 days), decision files, models |
| `POST /s1/replay` | `{ decision, file?, last? }` | recorded answers under changed rules: `{ replayed, same, changed, … }` |
| `POST /s1/compare` | `{ decision, models, last? }` | recorded states asked again per model |
| `GET /s1/proposals`, `POST /s1/proposals/{id}` | `{ decision: approve\|reject }` | the learner's proposals / applied or rejected |
| `GET /suggestions` | | suggestion outcomes by prompt version and model, latest 20 |
| `GET /memory?tier=` | | `{ memories, last_sleep, size }` (`short` default, `archived`, `all`, `long`) |
| `POST /memory/sleep?trigger=` | | tidies short-term memory now: `{ run, entries, kept, dropped, promoted, scorer, note }` |
| `POST /brain` | `{ action, … }` | the owner's `brain` actions (get, list, put, schema, lint, index) |
| `GET /skills` | | skills, drafts, made tools, with use and `sha` |
| `POST /skills/review` | `{ name, decision: accept\|reject, sha }` | `{ result }`; 400 if the draft changed since `sha` |
| `POST /tools/{name}/review` | `{ decision, sha }` | `{ result }`: approves (network allowed) or rejects a made tool |
| `GET /mcp` | | `{ config, servers, problems }` (no connecting) |
| `POST /mcp/login`, `/mcp/login/{flow}` | `{ server }` / `{ callback }` | the link to open / the finished sign-in (D-072) |
| `GET /mcp/pending`, `POST /mcp/pending/{id}` | the answer | MCP forms waiting for the owner / the response |
| `GET`/`POST /policy`, `POST /policy/undo` | `{ policy: { routes }, reason? }` | routing policy, evidence, suggestions / `{ version }` |
| `GET`/`POST /jobs` | `{ name, prompt, schedule, tz?, context?, skills?, model?, workspace? }` | `{ jobs, scheduler }` / the job |
| `PATCH`/`DELETE /jobs/{name}` | `{ enabled?, prompt?, schedule?, … }` | the job / `{ removed }` |
| `POST /jobs/{name}/run` | | `{ run, job }` (runs in background) |
| `GET /jobs/runs?job=&limit=` | | newest first (20, max 200): `{ id, job, trigger, status, session, started_at, ended_at, output, error }` |
| `GET /version?refresh=` | | running commit; is `main` ahead |
| `GET`/`POST /upgrade` | | upgrade progress / start one |

## WebSocket: `/api/sessions/{id}/ws`

The kernel pings after 30 s of silence. The client sends:

| Message | Effect |
|---|---|
| `{ "type": "prompt", "text", "mode"?: "queue"\|"steer", "suggestion"?: { "id", "taken" } }` | the owner's message; during a turn it waits (across restarts) for the turn's end, or with `steer` stops it at the next tool boundary (that action unrun) and goes first. `suggestion`: the one shown, `taken` if tab took it; recorded `accepted`, `edited`, `declined` or `unseen` |
| `{ "type": "abort" }` | stops the running turn |

Events, in order, typed as `zen_proto::client::Event` (`crates/zen-proto/src/client.rs`) with `Event::ends_turn` and echo matching. Unknown events are ignored.

| Event | Fields | Meaning |
|---|---|---|
| `message` | `message` | a message added (Pi's format, no `seq`: catch up with `?after=`). `kernel: true` on a user message: the kernel prompting the model, not the owner |
| `busy` | `busy: true, turn_id, harness, model, effort` | a turn started (kernel's too) |
| `delta`, `thinking` | `delta` | streamed answer / reasoning |
| `tool_start` | `call_id, name, args` | a tool call began (secrets masked, strings cut at 4 KB) |
| `tool_end` | `call_id, is_error, ms` | it finished |
| `end` | `error, cost, turn, next` | the turn ended; `turn`: the kernel's record (tokens, cost, time, `cache_break`, `context_tokens`, `render`); `cost`: session total (absent on a failed start); `next: true` (not sent today): wait for `idle` |
| `child_end` | `turn` | a verifier's turn ended; count its cost |
| `idle` | | reserved, not sent |
| `title` | `title` | the kernel named it |
| `suggestion` | `id, text` | the owner's likely next prompt; send `id` back with the next prompt |
| `model` | `model, effort` | a usage-limit failover moved the session's model (D-058); `effort` null: the default |
| `status` | `text` | progress: a long tool, a delegation, a summary, a failover, a queued prompt |
| `questions` | `questions: [{ question, options }]` | the `ask` tool's questions, recommended first |
| `taint` | `risky, tainted, source?, about?` | outside content read (`risky`) or a stop (`tainted`); trust clears both |
| `error` | `error` | a request failed |
| `resync` | `skipped, busy` | the client fell behind and missed events |
