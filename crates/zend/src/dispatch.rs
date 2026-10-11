//! Messages from workers: tool calls (run by the kernel), streamed text,
//! finished messages, usage reports and turn ends (`zen_proto::worker`'s `ToolCall` and `TurnEvent`).

use super::*;

use zen_proto::worker::{ToolCall, ToolResult, TurnEvent};

/// What a worker sent, read: a tool call to run, or a turn's notification.
enum FromWorker {
    Call(ToolCall),
    Event(TurnEvent),
}

/// The answer to a `tool.call` the kernel doesn't run.
fn refused(content: impl Into<String>) -> ToolResult {
    ToolResult { content: content.into(), is_error: true }
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
                if let Err(e) = handle_incoming(&app, worker, msg).await {
                    tracing::error!("handling tool call: {e:#}");
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
    let p = &msg.params;
    let id = session_id(p);
    if let Ok(id) = id {
        if !touch(app, id, worker, turn_id(p)).await {
            tracing::warn!("dropping `{}` for session {id}: not from the turn running on that worker", msg.method);
            if let Some(req_id) = msg.id {
                mind.respond(req_id, &refused("this turn has ended")).await?;
            }
            return Ok(());
        }
    }
    let read = if msg.method == ToolCall::METHOD {
        serde_json::from_value(p.clone()).map(FromWorker::Call)
    } else {
        TurnEvent::parse(&msg.method, p.clone()).map(FromWorker::Event)
    };
    match read {
        Ok(FromWorker::Call(call)) => {
            let id = match id {
                Ok(id) => id,
                Err(e) => {
                    if let Some(req_id) = msg.id {
                        mind.respond(req_id, &refused(format!("invalid session id: {e}"))).await?;
                    }
                    return Ok(());
                }
            };
            let ToolCall { name, call_id, args, .. } = call;
            let (mut cancel, model, turn_id, mut ending, workspace, kind) = {
                let mut turns = app.turns.lock().await;
                let Some(t) = turns.get_mut(&id) else {
                    drop(turns);
                    if let Some(req_id) = msg.id {
                        mind.respond(req_id, &refused("this turn has ended")).await?;
                    }
                    return Ok(());
                };
                t.tools_running += 1;
                (t.cancel.subscribe(), t.model.clone(), t.turn_id, t.ending, t.workspace.clone(), t.kind.clone())
            };
            let mut counted = ToolCounted { app: app.clone(), id, turn_id, done: false };
            // Commands can tell which session runs them (e.g. the commit-trailer hook in scripts/git-hooks).
            let session_env = id.to_string();
            let env = [("ZEN_SESSION_ID", session_env.as_str()), ("ZEN_MODEL", model.as_str())];
            // What clients see and the database keeps of the call: secrets masked, long strings cut.
            let (shown, full) = shown_args(&args);
            app.emit(id, json!({ "type": "tool_start", "call_id": call_id, "name": name, "args": shown })).await;
            let started = Instant::now();
            // Abort (or the turn ending) drops the tool future, which kills a running command.
            let run = async {
                if *cancel.borrow() {
                    return tools::ToolOutput { content: "not run: the turn was interrupted".into(), is_error: true };
                }
                tokio::select! {
                    out = async {
                        // Nothing more after a tool ended the turn; then the agent's own tools
                        // (agent.rs), or the kernel's built-ins.
                        let read_only = agent::read_only(kind.as_deref());
                        if let Some(why) = agent::refuse(ending) {
                            tools::ToolOutput { content: why, is_error: true }
                        } else if let Some(why) = agent::refusal(kind.as_deref(), &name) {
                            tools::ToolOutput { content: why, is_error: true }
                        } else if let Some(why) = agent::protected_refusal(&app.db, id, kind.as_deref(), &workspace, &name, &args).await {
                            tools::ToolOutput { content: why, is_error: true }
                        } else if let Some(why) = crate::workshop::mark_draft_write(app, id, &workspace, &name, &args).await {
                            tools::ToolOutput { content: why, is_error: true }
                        } else {
                            // A prompt file is backed up before the agent changes it, and the agent
                            // is told to compact it when the change takes it past its size.
                            let prompt_file = agent::prompt_file_target(&workspace, &name, &args);
                            if let Some(p) = &prompt_file {
                                if let Err(e) = crate::memory::backup_file(&crate::zen_home(), p) {
                                    tracing::warn!("backing up {}: {e:#}", p.display());
                                }
                            }
                            let mut out = if let Some(out) = agent::run_tool(app, id, &workspace, &name, &args, &mut ending).await {
                                out
                            } else {
                                // A verifier's shell is read-only; a tainted session's has no network.
                                let sandbox = if read_only {
                                    tools::Sandbox::ReadOnly
                                } else if name == "bash" {
                                    crate::taint::shell_sandbox(&app.db, id).await
                                } else {
                                    tools::Sandbox::None
                                };
                                let mut out = tools::execute(&workspace, &name, &args, &env, sandbox).await;
                                // Stored content a tainted session wrote is untrusted however it's read.
                                if name == "read" && !out.is_error {
                                    if let Some(source) = agent::read_of_tainted(&app.db, &workspace, &args).await {
                                        let about = args["path"].as_str().unwrap_or("");
                                        out = match crate::taint::hand_over(app, id, true, source, about, &out.content).await {
                                            Ok(content) => tools::ToolOutput { content, is_error: false },
                                            Err(e) => tools::ToolOutput { content: crate::taint::withheld(&e), is_error: true },
                                        };
                                    }
                                }
                                out
                            };
                            if let Some(note) = prompt_file.as_deref().filter(|_| !out.is_error).and_then(agent::over_cap_note) {
                                out.content.push_str(&note);
                            }
                            out
                        }
                    } => out,
                    _ = cancel.wait_for(|c| *c) => tools::ToolOutput { content: "interrupted: the turn was stopped before this tool finished".into(), is_error: true },
                }
            };
            // A tool that panics still gets an answer: the model must not wait for it.
            let mut out = match futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(run)).await {
                Ok(out) => out,
                Err(_) => {
                    tracing::error!("tool `{name}` panicked in session {id}");
                    tools::ToolOutput { content: format!("the `{name}` tool failed with an internal error in the kernel"), is_error: true }
                }
            };
            let ms = started.elapsed().as_millis() as i64;
            // First time this session touches a project with its own instructions: attach them.
            let new_context: Vec<PathBuf> = {
                // Up to 20 file checks: off the runtime's threads.
                let (ws, n, a) = (workspace.clone(), name.clone(), args.clone());
                let candidates = tokio::task::spawn_blocking(move || context::governing(&ws, &context::paths_in_call(&ws, &n, &a))).await.unwrap_or_default();
                let mut turns = app.turns.lock().await;
                counted.done = true;
                match turns.get_mut(&id).filter(|t| t.turn_id == turn_id) {
                    Some(t) => {
                        t.tools_running = t.tools_running.saturating_sub(1);
                        if ending.is_some() {
                            t.ending = ending;
                        }
                        t.last_activity = Instant::now();
                        candidates.into_iter().filter(|p| t.context.insert(p.clone())).collect()
                    }
                    None => Vec::new(),
                }
            };
            // Only a trusted repository's file is attached as instructions; any other is
            // untrusted content and taints the session (context.rs).
            let roots = if new_context.is_empty() { Vec::new() } else { context::trusted_roots(std::path::Path::new(&app.repo)) };
            for path in new_context {
                if let Some(text) = context::read_capped(&path) {
                    let trusted = context::trusted(&path, &roots);
                    if !trusted {
                        if let Err(e) = crate::taint::taint(app, id, "project file", &path.display().to_string()).await {
                            out.content.push_str(&format!("\n\n[instructions in {}: {}]", path.display(), crate::taint::withheld(&e)));
                            continue;
                        }
                    }
                    if let Err(e) = tape::append(&app.db, id, "context", &json!({ "path": path, "trusted": trusted })).await {
                        tracing::error!("recording instruction file {} for {id}: {e:#}", path.display());
                    }
                    out.content.push_str(&if trusted { context::attachment(&path, &text) } else { context::untrusted_attachment(&path, &text) });
                }
            }
            // Every tool's output, instruction files included, is masked before the model or the tape sees it.
            out.content = crate::secrets::mask_off_thread(std::mem::take(&mut out.content)).await;
            // Answer the worker first: a recording failure must not leave the model waiting.
            let output_bytes = out.content.len() as i64;
            if let Some(req_id) = msg.id {
                mind.respond(req_id, &ToolResult { content: out.content, is_error: out.is_error }).await?;
            }
            // Cut arguments keep the sha256 and size of the whole (masked) arguments under `_full`.
            let recorded = sqlx::query(
                "INSERT INTO tool_calls (session_id, call_id, name, args, is_error, duration_ms, output_bytes, turn_id)
                 VALUES ($1, $2, $3, CASE WHEN $9::text IS NULL THEN $4 ELSE $4 || jsonb_build_object('_full', jsonb_build_object(
                         'sha256', encode(sha256(convert_to($9, 'UTF8')), 'hex'), 'bytes', octet_length($9))) END, $5, $6, $7, $8)",
            )
            .bind(id)
            .bind(&call_id)
            .bind(&name)
            .bind(&shown)
            .bind(out.is_error)
            .bind(ms)
            .bind(output_bytes)
            .bind(turn_id)
            .bind(full)
            .execute(&app.db)
            .await;
            if let Err(e) = recorded {
                tracing::error!("recording tool call {call_id} for {id}: {e:#}");
            }
            app.emit(id, json!({ "type": "tool_end", "call_id": call_id, "is_error": out.is_error, "ms": ms })).await;
        }
        Ok(FromWorker::Event(TurnEvent::Delta { delta })) => {
            app.emit(id?, json!({ "type": "delta", "delta": delta })).await;
        }
        Ok(FromWorker::Event(TurnEvent::Thinking { delta })) => {
            app.emit(id?, json!({ "type": "thinking", "delta": delta })).await;
        }
        Ok(FromWorker::Event(TurnEvent::Message { message })) => {
            let id = id?;
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
        }
        Ok(FromWorker::Event(TurnEvent::Fallback { from, to, .. })) => {
            let id = id?;
            let or_unknown = |s: String| if s.is_empty() { "unknown".to_string() } else { s };
            let (from, to) = (or_unknown(from), or_unknown(to));
            tape::append(&app.db, id, "failover", &json!({ "from": from, "to": to, "reason": "hard_usage_limit" })).await?;
            app.emit(id, json!({ "type": "status", "text": format!("{from} usage limit reached; continuing with {to}") })).await;
        }
        Ok(FromWorker::Event(TurnEvent::Usage(report))) => {
            // The worker's report for the whole turn (engine, version, totals); recorded when the turn ends.
            let id = id?;
            if let Some(t) = app.turns.lock().await.get_mut(&id) {
                t.reported = Value::Object(report);
            }
        }
        Ok(FromWorker::Event(TurnEvent::End { error })) => {
            finish_turn(app, id?, json!(error)).await;
        }
        Err(e) => {
            let known = msg.method == ToolCall::METHOD || TurnEvent::parse(&msg.method, json!({})).is_ok();
            let why = if known { format!("invalid `{}` from worker: {e}", msg.method) } else { format!("unknown method {}", msg.method) };
            tracing::warn!("{why}");
            if let Some(req_id) = msg.id {
                mind.respond(req_id, &refused(why.clone())).await?;
            }
            // A turn must still end when its worker says so, however it said it.
            if msg.method == "turn.end" {
                finish_turn(app, id?, json!(why)).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
