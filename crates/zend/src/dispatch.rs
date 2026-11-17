//! Messages from workers: tool calls (run by the kernel), streamed text,
//! finished messages, usage reports and turn ends (`zen_proto::worker`'s `ToolCall` and `TurnEvent`).
//! Each tool call is recorded (masked arguments, output digest, error status) before the result
//! goes back to the worker.

use super::*;

use zen_proto::worker::{ToolCall, ToolResult, TurnEvent};

/// What a worker sent, read: a tool call to run, or a turn's notification.
enum FromWorker {
    Call(ToolCall),
    Event(TurnEvent),
}

/// The answer to a `tool.call` the kernel doesn't run.
fn refused(content: impl Into<String>) -> ToolResult {
    ToolResult { content: content.into(), content_blocks: Vec::new(), is_error: true }
}

/// A session's ordered queue of worker notifications.
type Queues = Arc<std::sync::Mutex<HashMap<Uuid, tokio::sync::mpsc::UnboundedSender<(usize, Incoming)>>>>;

/// How long a session's queue task waits for more messages before it ends.
const QUEUE_IDLE: Duration = Duration::from_secs(60);

pub(crate) async fn dispatch(app: AppState, mut incoming: tokio::sync::mpsc::UnboundedReceiver<(usize, Incoming)>) {
    // Notifications are handled in arrival order per session, so each session's stream and tape
    // stay ordered, while one session's slow write (a turn's end, a model refresh) doesn't hold up
    // the others. Tool calls run concurrently; the worker already emitted the assistant message
    // that requested them. Messages without a session are handled here, in order.
    let queues: Queues = Arc::default();
    while let Some((worker, msg)) = incoming.recv().await {
        if msg.method == ToolCall::METHOD {
            let app = app.clone();
            tokio::spawn(async move {
                let request = msg.id.clone();
                if let Err(e) = handle_incoming(&app, worker, msg).await {
                    tracing::error!("handling tool call: {e:#}");
                    if let Some(request) = request {
                        let _ = app.workers[worker].mind().respond(request, &refused("tool failed: kernel could not complete its bookkeeping")).await;
                    }
                }
            });
            continue;
        }
        match session_id(&msg.params) {
            Ok(id) => enqueue(&app, &queues, id, (worker, msg)),
            Err(_) => {
                if let Err(e) = handle_incoming(&app, worker, msg).await {
                    tracing::error!("handling mind message: {e:#}");
                }
            }
        }
    }
}

/// Hand a message to its session's queue, starting the queue's task if there is none (or it just
/// ended). Sending under the map's lock keeps the order: a task only ends after it found its
/// queue empty while holding the same lock, and then removes itself from the map.
fn enqueue(app: &AppState, queues: &Queues, id: Uuid, item: (usize, Incoming)) {
    let mut map = queues.lock().unwrap_or_else(|e| e.into_inner());
    let item = match map.get(&id) {
        Some(tx) => match tx.send(item) {
            Ok(()) => return,
            Err(e) => e.0,
        },
        None => item,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(usize, Incoming)>();
    let _ = tx.send(item);
    map.insert(id, tx);
    let (app, queues) = (app.clone(), queues.clone());
    tokio::spawn(async move {
        loop {
            let next = match tokio::time::timeout(QUEUE_IDLE, rx.recv()).await {
                Ok(Some(m)) => Some(m),
                Ok(None) => None,
                Err(_) => {
                    let mut map = queues.lock().unwrap_or_else(|e| e.into_inner());
                    match rx.try_recv() {
                        Ok(m) => Some(m),
                        Err(_) => {
                            map.remove(&id);
                            None
                        }
                    }
                }
            };
            let Some((worker, msg)) = next else { break };
            if let Err(e) = handle_incoming(&app, worker, msg).await {
                tracing::error!("handling mind message for {id}: {e:#}");
            }
        }
    });
}

pub(crate) fn session_id(params: &Value) -> Result<Uuid> {
    Ok(params.get("session_id").and_then(Value::as_str).unwrap_or_default().parse()?)
}

/// The turn a worker message names (`turn_id`, echoed from `turn.start`); None if it names none.
pub(crate) fn turn_id(params: &Value) -> Option<Uuid> {
    params.get("turn_id").and_then(Value::as_str).and_then(|t| t.parse().ok())
}

/// Note activity on a session's turn. Returns false when the message doesn't belong to the turn
/// running on that worker (e.g. a late message from a turn the kernel already ended, while the
/// session's next turn runs); such messages are dropped so they can't leak into the session.
/// A message without a `turn_id` (a worker that predates it) is matched by session and worker only.
pub(crate) async fn touch(app: &App, id: Uuid, worker: usize, turn: Option<Uuid>) -> bool {
    match app.turns.lock().await.get_mut(&id) {
        Some(t) if t.worker == worker && turn.is_none_or(|turn| turn == t.turn_id) => {
            t.last_activity = Instant::now();
            true
        }
        _ => false,
    }
}

/// A tool call counted in its turn's `tools_running` (the watchdog leaves a turn running a tool
/// alone). If the call ends without uncounting itself (a panic), dropping this does it.
struct ToolCounted {
    app: AppState,
    id: Uuid,
    turn_id: Uuid,
    done: bool,
}

impl Drop for ToolCounted {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let (app, id, turn_id) = (self.app.clone(), self.id, self.turn_id);
        tokio::spawn(async move {
            if let Some(t) = app.turns.lock().await.get_mut(&id).filter(|t| t.turn_id == turn_id) {
                t.tools_running = t.tools_running.saturating_sub(1);
            }
        });
    }
}

/// The longest a string in a tool call's arguments is kept (clients, `tool_calls.args`).
const ARG_CAP: usize = 4096;

/// A tool call's arguments as clients see them and the database keeps them: every string masked
/// (the model may echo a secret into a command) and cut at ARG_CAP (a `write` carries a whole file).
/// Also returns the whole masked arguments as text when something was cut, to keep their hash.
pub(crate) fn shown_args(args: &Value) -> (Value, Option<String>) {
    /// Every string masked (`cap` None), or cut at `cap`; `cut` is set when one was.
    fn walk(v: &Value, cap: Option<usize>, cut: &mut bool) -> Value {
        match v {
            Value::String(s) => match cap {
                None => json!(crate::secrets::mask(s)),
                Some(cap) if s.len() > cap => {
                    *cut = true;
                    json!(format!("{}… [cut: {} bytes]", &s[..s.floor_char_boundary(cap)], s.len()))
                }
                Some(_) => v.clone(),
            },
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, cap, cut)).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), walk(x, cap, cut))).collect()),
            other => other.clone(),
        }
    }
    let mut cut = false;
    let masked = walk(args, None, &mut cut);
    let shown = walk(&masked, Some(ARG_CAP), &mut cut);
    let full = cut.then(|| masked.to_string());
    (shown, full)
}

pub(crate) async fn handle_incoming(app: &AppState, worker: usize, msg: Incoming) -> Result<()> {
    let mind = app.workers[worker].mind();
    let Incoming { id: request, method, params } = msg;
    let id = session_id(&params);
    if let Ok(id) = id {
        if !touch(app, id, worker, turn_id(&params)).await {
            tracing::warn!("dropping `{method}` for session {id}: not from the turn running on that worker");
            if let Some(request) = request {
                mind.respond(request, &refused("this turn has ended")).await?;
            }
            return Ok(());
        }
    }
    // The params are moved into what they describe: a streamed delta or a message isn't copied.
    let read = if method == ToolCall::METHOD {
        serde_json::from_value(params).map(FromWorker::Call)
    } else {
        TurnEvent::parse(&method, params).map(FromWorker::Event)
    };
    match read {
        Ok(FromWorker::Call(call)) => match id {
            Ok(id) => run_call(app, &mind, id, request, call).await?,
            Err(e) => {
                if let Some(request) = request {
                    mind.respond(request, &refused(format!("invalid session id: {e}"))).await?;
                }
            }
        },
        Ok(FromWorker::Event(TurnEvent::Delta { delta })) => {
            app.emit(id?, json!({ "type": "delta", "delta": delta })).await;
        }
        Ok(FromWorker::Event(TurnEvent::Thinking { delta })) => {
            app.emit(id?, json!({ "type": "thinking", "delta": delta })).await;
        }
        Ok(FromWorker::Event(TurnEvent::Message { message })) => record_message(app, id?, message).await?,
        Ok(FromWorker::Event(TurnEvent::Fallback { from, to, .. })) => fail_over(app, id?, from, to).await?,
        Ok(FromWorker::Event(TurnEvent::Usage(report))) => {
            // The worker's report for the whole turn (engine, version, totals); recorded when the turn ends.
            if let Some(t) = app.turns.lock().await.get_mut(&id?) {
                t.reported = Value::Object(report);
            }
        }
        Ok(FromWorker::Event(TurnEvent::End { error })) => {
            finish_turn(app, id?, json!(error)).await;
        }
        Err(e) => {
            let known = method == ToolCall::METHOD || TurnEvent::parse(&method, json!({})).is_ok();
            let why = if known { format!("invalid `{method}` from worker: {e}") } else { format!("unknown method {method}") };
            tracing::warn!("{why}");
            if let Some(request) = request {
                mind.respond(request, &refused(why.clone())).await?;
            }
            // A turn must still end when its worker says so, however it said it.
            if method == "turn.end" {
                finish_turn(app, id?, json!(why)).await;
            }
        }
    }
    Ok(())
}

/// What a tool call needs of its turn, read once when it starts.
struct CallTurn {
    model: String,
    turn_id: Uuid,
    workspace: PathBuf,
    kind: Option<String>,
    agent: crate::agents::Agent,
}

/// Run one tool call of session `id` and answer `request`: refused while the owner steers or once
/// its turn ended; otherwise run in order with the turn's other calls, its output bounded and
/// masked, recorded in `tool_calls`, and shown to clients as it starts and ends.
async fn run_call(app: &AppState, mind: &Mind, id: Uuid, request: Option<Value>, call: ToolCall) -> Result<()> {
    let steer: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pending_prompts WHERE session_id=$1 AND steer AND error IS NULL)")
        .bind(id)
        .fetch_one(&app.db)
        .await?;
    if steer {
        if let Some(request) = request {
            mind.respond(request, &refused("owner steering requested; this action was not run")).await?;
        }
        let app = app.clone();
        tokio::spawn(async move { abort_turn(&app, id).await });
        return Ok(());
    }
    let ToolCall { name, call_id, args, .. } = call;
    let (turn, cancel, ending) = {
        let mut turns = app.turns.lock().await;
        let Some(t) = turns.get_mut(&id) else {
            drop(turns);
            if let Some(request) = request {
                mind.respond(request, &refused("this turn has ended")).await?;
            }
            return Ok(());
        };
        t.tools_running += 1;
        let turn = CallTurn { model: t.model.clone(), turn_id: t.turn_id, workspace: t.workspace.clone(), kind: t.kind.clone(), agent: t.agent.clone() };
        (turn, t.cancel.subscribe(), t.ending)
    };
    let turn_id = turn.turn_id;
    let mut counted = ToolCounted { app: app.clone(), id, turn_id, done: false };
    // What clients see and the database keeps of the call: secrets masked, long strings cut.
    let (shown, full) = shown_args(&args);
    app.emit(id, json!({ "type": "tool_start", "call_id": call_id, "name": name, "args": shown })).await;
    let started = Instant::now();
    let (mut out, ending) = run_in_order(app, id, &turn, cancel, ending, &name, &args).await;
    let ms = started.elapsed().as_millis() as i64;
    // First time this session touches a project with its own instructions: attach them.
    let new_context: Vec<PathBuf> = {
        // Up to 20 file checks: off the runtime's threads.
        let (workspace, n, a) = (turn.workspace.clone(), name.clone(), args.clone());
        let candidates = tokio::task::spawn_blocking(move || context::governing(&workspace, &context::paths_in_call(&workspace, &n, &a))).await.unwrap_or_default();
        let mut turns = app.turns.lock().await;
        counted.done = true;
        match turns.get_mut(&id).filter(|t| t.turn_id == turn_id) {
            Some(t) => {
                t.tools_running = t.tools_running.saturating_sub(1);
                if ending.is_some() {
                    t.ending = ending;
                }
                if note_failure(&mut t.failures, &name, &out) && t.ending.is_none() {
                    t.ending = Some(REPEATED);
                    out.content.push_str(&repeated_note(&name));
                }
                t.last_activity = Instant::now();
                candidates.into_iter().filter(|p| t.context.insert(p.clone())).collect()
            }
            None => Vec::new(),
        }
    };
    let sandbox = if new_context.is_empty() { None } else { turn.agent.sandbox() };
    for path in new_context {
        let (p, s) = (path.clone(), sandbox.clone());
        let Ok(Some(text)) = tokio::task::spawn_blocking(move || context::read_for(&p, s.as_ref())).await else { continue };
        if let Err(e) = tape::append(&app.db, id, "context", &json!({ "path": path })).await {
            tracing::error!("recording instruction file {} for {id}: {e:#}", path.display());
        }
        out.content.push_str(&context::attachment(&path, &context::capped(text)));
    }
    bound_output(&mut out).await;
    let output_bytes = (out.content.len() + out.content_blocks.iter().map(|b| b.to_string().len()).sum::<usize>()) as i64;
    let result = ToolResult { content: out.content, content_blocks: out.content_blocks, is_error: out.is_error };
    // Cut arguments keep the sha256 and size of the whole (masked) arguments under `_full`.
    let recorded = sqlx::query(
        "INSERT INTO tool_calls (session_id, call_id, name, args, is_error, duration_ms, output_bytes, turn_id, result_sha256)
         VALUES ($1, $2, $3, CASE WHEN $9 IS NULL THEN $4 ELSE json_patch($4, json_object('_full', json_object(
                 'sha256', sha256_hex($9), 'bytes', length(CAST($9 AS BLOB))))) END, $5, $6, $7, $8, sha256_hex($10))",
    )
    .bind(id)
    .bind(&call_id)
    .bind(&name)
    .bind(&shown)
    .bind(result.is_error)
    .bind(ms)
    .bind(output_bytes)
    .bind(turn_id)
    .bind(full)
    // The digest of exactly what the worker will write to the tape (tape.rs checks it).
    .bind(zen_proto::digest_text(&Value::Array(result.blocks())))
    .execute(&app.db)
    .await;
    if let Err(e) = recorded {
        tracing::error!("recording tool call {call_id} for {id}: {e:#}");
    }
    if let Some(request) = request {
        mind.respond(request, &result).await?;
    }
    app.emit(id, json!({ "type": "tool_end", "call_id": call_id, "is_error": result.is_error, "ms": ms })).await;
    Ok(())
}

/// How many times one tool may fail the same way in a turn before the turn's later tool calls are
/// refused. An unattended run once retried a delegate 463 times at a usage limit, rewriting a
/// growing log between tries, until its tape no longer fit any model's window.
const REPEATED_FAILURES: u32 = 5;
const REPEATED: &str = "the same tool failure repeated";

fn repeated_note(name: &str) -> String {
    format!(
        "\n\n[zen] `{name}` has failed this way {REPEATED_FAILURES} times in this turn; further tool calls are refused. \
         End your turn now: say what failed, what was done and what is left."
    )
}

/// What makes two failures "the same": the tool, and the end of its error text (where the cause
/// usually is) with runs of digits folded, so ids, times and counts don't tell retries apart.
fn failure_signature(name: &str, content: &str) -> String {
    // Fold first, then cut: a longer number must not shift what the window sees.
    let chars: Vec<char> = content.chars().collect();
    let mut folded = String::new();
    for &c in &chars[chars.len().saturating_sub(600)..] {
        let c = if c.is_ascii_digit() { '#' } else if c.is_whitespace() { ' ' } else { c };
        if !(matches!(c, '#' | ' ') && folded.ends_with(c)) {
            folded.push(c);
        }
    }
    let tail: Vec<char> = folded.chars().collect();
    format!("{name}\0{}", tail[tail.len().saturating_sub(200)..].iter().collect::<String>())
}

/// Count a call's outcome: a failure adds to its signature's count, a success of the same tool
/// clears that tool's counts. True when this failure reaches REPEATED_FAILURES.
fn note_failure(failures: &mut HashMap<String, u32>, name: &str, out: &tools::ToolOutput) -> bool {
    if !out.is_error {
        let prefix = format!("{name}\0");
        failures.retain(|k, _| !k.starts_with(&prefix));
        return false;
    }
    let n = failures.entry(failure_signature(name, &out.content)).or_insert(0);
    *n += 1;
    *n == REPEATED_FAILURES
}

/// Run a call once the turn's earlier calls allow it: read-only calls may overlap, any other runs
/// alone, after those before it. Abort (or the turn ending) drops the tool future, which kills a
/// running command; a tool that panics still gets an answer, so the model doesn't wait for it.
/// Returns the output and the turn's `ending` as the call left it.
async fn run_in_order(
    app: &AppState,
    id: Uuid,
    turn: &CallTurn,
    mut cancel: watch::Receiver<bool>,
    mut ending: Option<&'static str>,
    name: &str,
    args: &Value,
) -> (tools::ToolOutput, Option<&'static str>) {
    let order = app.tool_order.lock().await.entry(id).or_default().clone();
    let run = async {
        let _order: Box<dyn std::any::Any + Send> = if zen_proto::read_only_tool(name) {
            Box::new(order.read_owned().await)
        } else {
            Box::new(order.write_owned().await)
        };
        if *cancel.borrow() {
            return tools::ToolOutput::error("not run: the turn was interrupted");
        }
        tokio::select! {
            out = run_tool(app, id, turn, name, args, &mut ending) => out,
            _ = cancel.wait_for(|c| *c) => tools::ToolOutput::error("interrupted: the turn was stopped before this tool finished"),
        }
    };
    let out = match futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(run)).await {
        Ok(out) => out,
        Err(_) => {
            tracing::error!("tool `{name}` panicked in session {id}");
            tools::ToolOutput::error(format!("the `{name}` tool failed with an internal error in the kernel"))
        }
    };
    (out, ending)
}

/// One tool, unless it's refused: nothing more after a tool ended the turn; then the agent's own
/// tools (agent.rs), or the kernel's built-ins. A prompt file is backed up before the agent changes
/// it, and the agent is told to compact it when the change takes it past its size.
async fn run_tool(app: &AppState, id: Uuid, turn: &CallTurn, name: &str, args: &Value, ending: &mut Option<&'static str>) -> tools::ToolOutput {
    let workspace = turn.workspace.as_path();
    let why = agent::refusal(turn.kind.as_deref(), name)
        .or_else(|| turn.agent.refusal(name))
        .or_else(|| agent::refuse(*ending))
        .or_else(|| agent::record_refusal(workspace, name, args))
        .or_else(|| {
            (turn.kind.as_deref() == Some(crate::review::REVIEW))
                .then(|| crate::review::write_refusal(&crate::zen_home(), &turn.agent.name, &[turn.agent.skill_home()], workspace, name, args))
                .flatten()
        });
    if let Some(why) = why {
        return tools::ToolOutput::error(why);
    }
    let prompt_file = agent::prompt_file_target(workspace, name, args);
    if let Some(p) = &prompt_file {
        if let Err(e) = crate::memory::backup_file(&crate::zen_home(), p) {
            tracing::warn!("backing up {}: {e:#}", p.display());
        }
    }
    let mut out = match agent::run_tool(app, id, workspace, &turn.agent, name, args, ending).await {
        Some(out) => out,
        None => {
            // Commands can tell which session runs them (e.g. the commit-trailer hook in scripts/git-hooks).
            let session = id.to_string();
            let sandbox = turn.agent.sandbox();
            tools::execute(workspace, name, args, &[("ZEN_SESSION_ID", session.as_str()), ("ZEN_MODEL", turn.model.as_str())], sandbox.as_ref()).await
        }
    };
    if let Some(note) = prompt_file.as_deref().filter(|_| !out.is_error).and_then(agent::over_cap_note) {
        out.content.push_str(&note);
    }
    out
}

/// Make a tool's output safe to hand back: every text masked (instruction files included), media
/// blocks validated (an invalid one withholds the result), text blocks cut to the preview budget,
/// and text past the result size saved to a file with a preview in its place. An error keeps no
/// media: it never forwards a partially authorized image.
async fn bound_output(out: &mut tools::ToolOutput) {
    out.content = crate::secrets::mask_off_thread(std::mem::take(&mut out.content)).await;
    // Text blocks are masked before any cut; binary blocks are validated without reading their
    // base64 as text.
    if !out.is_error {
        let invalid = out.content_blocks.iter().any(|b| match b["type"].as_str() {
            Some("text") => !b["text"].is_string(),
            Some("image") => !crate::media::validate_image_block(b),
            _ => true,
        });
        if invalid {
            out.content = "result withheld: invalid or unsupported media block".into();
            out.is_error = true;
        }
        let mut remaining = crate::output::PREVIEW_BYTES;
        out.content_blocks.retain_mut(|block| {
            if let Some(text) = block["text"].as_str() {
                if remaining == 0 {
                    return false;
                }
                let text = crate::secrets::mask(text);
                let end = text.floor_char_boundary(remaining.min(text.len()));
                let bounded = format!("{}{}", &text[..end], if end < text.len() { " [text block truncated]" } else { "" });
                remaining = remaining.saturating_sub(end);
                block["text"] = json!(bounded);
            }
            true
        });
    }
    if let Some(preview) = crate::output::preview_at(&out.content, crate::output::RESULT_BYTES) {
        match crate::tools::save_full_output(&out.content) {
            Some(path) => {
                out.content = format!("{preview}\n[full captured result saved to {}]", path.display());
                out.saved_outputs.push(path);
            }
            None => out.content = format!("{preview}\n[omitted result could not be saved]"),
        }
    }
    if out.is_error {
        out.content_blocks.clear();
    }
}

/// A finished message: onto the tape, an assistant's model call into `model_calls` (with the
/// model the provider reported kept for the turn), then to clients.
async fn record_message(app: &AppState, id: Uuid, message: Value) -> Result<()> {
    tape::append(&app.db, id, "message", &message).await?;
    if message.get("role").and_then(Value::as_str) == Some("assistant") {
        let u = &message["usage"];
        let n = |k: &str| u.get(k).and_then(Value::as_i64).unwrap_or(0);
        let turn_id = app.turns.lock().await.get_mut(&id).map(|t| {
            if let Some(m) = message["model"].as_str().filter(|m| !m.is_empty()) {
                t.model_resolved = Some(m.to_string());
            }
            t.turn_id
        });
        sqlx::query(
            "INSERT INTO model_calls (session_id, provider, model, input_tokens, output_tokens, cache_read, cache_write, cost_usd, stop_reason, turn_id, duration_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(id)
        .bind(message["provider"].as_str().unwrap_or(""))
        .bind(message["model"].as_str().unwrap_or(""))
        .bind(n("input"))
        .bind(n("output"))
        .bind(n("cacheRead"))
        .bind(n("cacheWrite"))
        .bind(u["cost"]["total"].as_f64().unwrap_or(0.0))
        .bind(message["stopReason"].as_str())
        .bind(turn_id)
        .bind(message["durationMs"].as_i64())
        .execute(&app.db)
        .await?;
    }
    app.emit(id, json!({ "type": "message", "message": message })).await;
    Ok(())
}

/// The worker moved the turn to another provider (a usage limit): on the tape, told to clients,
/// and the session stays on the model it failed over to until the owner picks another (its next
/// turns, tools' ZEN_MODEL and summaries use it, and clients show it).
async fn fail_over(app: &AppState, id: Uuid, from: String, to: String) -> Result<()> {
    let or_unknown = |s: String| if s.is_empty() { "unknown".to_string() } else { s };
    let (from, to) = (or_unknown(from), or_unknown(to));
    tape::append(&app.db, id, "failover", &json!({ "from": from, "to": to, "reason": "hard_usage_limit" })).await?;
    app.emit(id, json!({ "type": "status", "text": format!("{from} usage limit reached; continuing with {to}") })).await;
    if to == "unknown" {
        return Ok(());
    }
    if let Some(t) = app.turns.lock().await.get_mut(&id) {
        t.model = to.clone();
    }
    let effort: Option<String> = sqlx::query_scalar("SELECT effort FROM sessions WHERE id = $1").bind(id).fetch_one(&app.db).await?;
    let effort = effort_on(app, &to, effort).await;
    sqlx::query("UPDATE sessions SET model = $2, effort = $3, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1")
        .bind(id)
        .bind(&to)
        .bind(&effort)
        .execute(&app.db)
        .await?;
    app.emit(id, json!({ "type": "model", "model": to, "effort": effort })).await;
    Ok(())
}

/// External shared-brain calls run kernel tools with a real session and the same masking,
/// output bounding, serialization and SQLite audit as worker calls. No worker turn is invented.
pub(crate) async fn external_brain_call(app: &AppState, id: Uuid, who: &crate::agents::Agent, name: &str, args: &Value, permitted: bool) -> Result<Value> {
    let order = app.tool_order.lock().await.entry(id).or_default().clone();
    let _guard = order.write_owned().await;
    let call_id = format!("mcp-{}", Uuid::new_v4());
    let started = Instant::now();
    let mut out = if !permitted {
        tools::ToolOutput::error("tool or action is not permitted by this shared-brain session")
    } else if name == "wiki_read" {
        match crate::shared_brain::read_wiki(who, args) {
            Ok(text) => tools::ToolOutput::text(text, false),
            Err(e) => tools::ToolOutput::error(format!("wiki read refused: {e}")),
        }
    } else {
        let turn = CallTurn { model: app.default_model.clone(), turn_id: Uuid::new_v4(), workspace: app.workspace.clone(), kind: None, agent: who.clone() };
        run_tool(app, id, &turn, name, args, &mut None).await
    };
    bound_output(&mut out).await;
    let (shown, full) = shown_args(args);
    let result = json!({"content":[{"type":"text","text":out.content}],"isError":out.is_error});
    sqlx::query("INSERT INTO tool_calls (session_id,call_id,name,args,is_error,duration_ms,output_bytes,result_sha256) VALUES ($1,$2,$3,CASE WHEN $8 IS NULL THEN $4 ELSE json_patch($4,json_object('_full',json_object('sha256',sha256_hex($8),'bytes',length(CAST($8 AS BLOB))))) END,$5,$6,$7,sha256_hex($9))")
        .bind(id).bind(&call_id).bind(name).bind(&shown)
        .bind(out.is_error).bind(started.elapsed().as_millis() as i64).bind(result.to_string().len() as i64)
        .bind(full).bind(zen_proto::digest_text(&result["content"])).execute(&app.db).await?;
    tape::append(&app.db, id, "mcp_call", &json!({"call_id":call_id,"name":name,"args":shown,"result":result})).await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overnight loop's failures (subagent ids, models, times differ) count as one; another
    /// error, or a success of the same tool in between, doesn't add to it.
    #[test]
    fn the_same_failure_repeated_ends_the_turn() {
        let limit = |sub: &str, model: &str, at: &str| {
            tools::ToolOutput::error(format!(
                "Subagent {sub} on codex/{model} finished in 2s, $0.000, with an error: You’ve hit your usage limit. Upgrade to Pro \
                 (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again at {at}.:\n(no answer)"
            ))
        };
        let mut f = HashMap::new();
        assert!(!note_failure(&mut f, "delegate", &limit("4b0d6287", "gpt-6.1-terra", "5:20 AM")));
        assert!(!note_failure(&mut f, "write", &tools::ToolOutput::text("wrote 140346 bytes", false)), "another tool's success");
        assert!(!note_failure(&mut f, "delegate", &tools::ToolOutput::error("Subagent x failed: no such model")), "another error");
        assert!(!note_failure(&mut f, "delegate", &limit("12419ef9", "gpt-6.1-sol", "5:20 AM")));
        assert!(!note_failure(&mut f, "delegate", &limit("5e94bd96", "gpt-6.1-sol", "10:05 AM")));
        assert!(!note_failure(&mut f, "delegate", &limit("aa", "gpt-6.1-terra", "5:21 AM")));
        assert!(note_failure(&mut f, "delegate", &limit("bb", "gpt-6.1-sol", "5:22 AM")), "the fifth");
        assert!(!note_failure(&mut f, "delegate", &limit("cc", "gpt-6.1-sol", "5:22 AM")), "said once");
        assert!(!note_failure(&mut f, "delegate", &tools::ToolOutput::text("done", false)));
        assert!(f.is_empty(), "a success of the tool clears its counts");
    }

    #[test]
    fn tool_arguments_are_masked_and_cut() {
        let big = "x".repeat(10_000);
        let (shown, full) = shown_args(&json!({ "command": "echo ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789", "n": 3, "files": [{ "content": big }] }));
        assert!(!shown.to_string().contains("AbCdEfGhIjKlMnOp"), "a token the model typed is masked");
        assert_eq!(shown["n"], 3);
        let content = shown["files"][0]["content"].as_str().unwrap();
        assert!(content.len() < 4200 && content.ends_with("[cut: 10000 bytes]"));
        let full = full.expect("the whole arguments, for their hash");
        assert!(full.contains(&big) && !full.contains("AbCdEfGhIjKlMnOp"));
        let (small, none) = shown_args(&json!({ "path": "a.txt" }));
        assert_eq!((small, none), (json!({ "path": "a.txt" }), None), "nothing cut: no hash");
    }
}
