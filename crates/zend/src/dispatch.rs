//! Messages from workers: tool calls (run by the kernel), streamed text,
//! finished messages, usage reports and turn ends (`zen_proto::worker`'s `ToolCall` and `TurnEvent`).
//! Tool source risk, full masked query/focus, output digest and error status are recorded before
//! responding. Tape provenance binds to that exact record, not to worker-supplied claims.

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
                    return tools::ToolOutput { rating: None, content: "not run: the turn was interrupted".into(), is_error: true };
                }
                tokio::select! {
                    out = async {
                        // Nothing more after a tool ended the turn; then the agent's own tools
                        // (agent.rs), or the kernel's built-ins.
                        let read_only = agent::read_only(kind.as_deref());
                        if crate::taint::stopped(&app.db,id).await.unwrap_or(true) {
                            return tools::ToolOutput { rating: None, content: "not run: session stopped by contamination; owner review required".into(), is_error: true };
                        }
                        // Immutable owner-only and kind boundaries cannot be overridden by a grant.
                        if let Some(why) = agent::refusal(kind.as_deref(), &name) {
                            return tools::ToolOutput { rating: None, content: why, is_error: true };
                        }
                        if let Some(why) = agent::protected_refusal(&app.db, id, kind.as_deref(), &workspace, &name, &args, true).await {
                            return tools::ToolOutput { rating: None, content: why, is_error: true };
                        }
                        // A shell's view is rated (and its masks found) before the gate decides on it.
                        let view = if matches!(name.as_str(), "bash" | "verify") {
                            let view = match crate::fsview::prepare(&app.db, &workspace).await {
                                Ok(v) if name == "verify" => v.rate_all(&app.db, id).await.map(|_| v),
                                v => v,
                            };
                            match view {
                                Ok(v) => v,
                                Err(e) => return tools::ToolOutput { rating: None, content: format!("not run: the shell's view couldn't be checked ({e:#})"), is_error: true },
                            }
                        } else { crate::fsview::View::default() };
                        let policy = if read_only { Ok(crate::gate::Policy::Run) }
                            else { crate::gate::check(app, id, &workspace, &name, &args).await };
                        let policy = match policy {
                            Ok(p) => p,
                            Err(e) => return tools::ToolOutput { rating: None, content: format!("not run: action gate unavailable ({e})"), is_error: true },
                        };
                        if let crate::gate::Policy::Hold(why) | crate::gate::Policy::Lock(why) = &policy {
                            return tools::ToolOutput { rating: None, content: crate::gate::held_note(why), is_error: true };
                        }
                        if let Some(why) = agent::refuse(ending) {
                            tools::ToolOutput { rating: None, content: why, is_error: true }
                        } else if let Some(why) = agent::refusal(kind.as_deref(), &name) {
                            tools::ToolOutput { rating: None, content: why, is_error: true }
                        } else if let Some(why) = agent::protected_refusal(&app.db, id, kind.as_deref(), &workspace, &name, &args, matches!(policy, crate::gate::Policy::Granted)).await {
                            tools::ToolOutput { rating: None, content: why, is_error: true }
                        } else if let Some(why) = crate::workshop::mark_draft_write(app, id, &workspace, &name, &args).await {
                            tools::ToolOutput { rating: None, content: why, is_error: true }
                        } else {
                            // A prompt file is backed up before the agent changes it, and the agent
                            // is told to compact it when the change takes it past its size.
                            let prompt_file = agent::prompt_file_target(&workspace, &name, &args);
                            if let Some(p) = &prompt_file {
                                if let Err(e) = crate::memory::backup_file(&crate::zen_home(), p) {
                                    tracing::warn!("backing up {}: {e:#}", p.display());
                                }
                            }
                            let mut out = if let Some(out) = agent::run_tool(app, id, &workspace, &name, &args, &mut ending, &policy).await {
                                out
                            } else {
                                // A verifier's shell is read-only; every other shell is contained
                                // (D-067), except a `host: true` command the gate or the owner let run.
                                let sandbox = if read_only {
                                    tools::Sandbox::ReadOnly
                                } else if name == "bash" && args["host"] == true {
                                    tools::Sandbox::None
                                } else if matches!(name.as_str(), "write" | "edit") {
                                    if crate::taint::tainted(&app.db, id).await { tools::Sandbox::GatedFile } else { tools::Sandbox::None }
                                } else {
                                    crate::gate::shell_mode(&app.db, id).await
                                };
                                if let tools::Sandbox::Contained { net: true, .. } = sandbox {
                                    if let Err(e) = crate::proxy::ensure(&app.db, id).await {
                                        tracing::warn!("egress proxy for {id}: {e:#}");
                                    }
                                }
                                let execute_args = args.clone();
                                let proxy_since = Instant::now();
                                let start = std::time::SystemTime::now();
                                let mut out = tools::execute_origin(&workspace, &name, &execute_args, &env, sandbox, Some(tools::Origin {db:&app.db,session:id,masks:&view.masks})).await;
                                if name == "bash" {
                                    // What the output shows of rated files rates it, as a `read` would;
                                    // so do the hosts the command reached through the proxy (D-067).
                                    let (risk, writers) = view.exposure(&out.content);
                                    let risk = risk.max(crate::proxy::contacted_since(id, proxy_since).unwrap_or(1));
                                    let shown = async {
                                        crate::fsview::finish(&app.db, id, &workspace, start, sandbox == tools::Sandbox::None).await?;
                                        crate::fsview::record(&app.db, id, risk, &writers).await?;
                                        crate::taint::hand_over_rating(app, id, risk, "shell", args["command"].as_str().unwrap_or(""), &out.content).await
                                    };
                                    match shown.await {
                                        Ok(content) => out.content = content,
                                        Err(e) => out = tools::ToolOutput { rating: None, content: format!("{e:#}"), is_error: true },
                                    }
                                }
                                // Stored content a tainted session wrote is untrusted however it's read.
                                if name == "read" && !out.is_error {
                                    if let Some(rating) = &out.rating {
                                        let risk = rating.risk;
                                        out.content = match crate::taint::hand_over_rating(app,id,risk,"file",args["path"].as_str().unwrap_or(""),&out.content).await {
                                            Ok(content) => content,
                                            Err(e) => {out.is_error=true; crate::taint::withheld(&e)}
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
                    _ = cancel.wait_for(|c| *c) => tools::ToolOutput { rating: None, content: "interrupted: the turn was stopped before this tool finished".into(), is_error: true },
                }
            };
            // A tool that panics still gets an answer: the model must not wait for it.
            let mut out = match futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(run)).await {
                Ok(out) => out,
                Err(_) => {
                    tracing::error!("tool `{name}` panicked in session {id}");
                    tools::ToolOutput { rating: None, content: format!("the `{name}` tool failed with an internal error in the kernel"), is_error: true }
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
            // Only a trusted repository's file whose exact bytes are rated 0-1 is attached as
            // instructions; any other is handed over at its rating (context.rs, D-062).
            let roots = if new_context.is_empty() { Vec::new() } else { context::trusted_roots(std::path::Path::new(&app.repo)) };
            for path in new_context {
                let Ok(text) = tokio::fs::read_to_string(&path).await else { continue };
                let shown = path.display().to_string();
                let in_trusted = context::trusted(&path, &roots);
                let repo = if in_trusted { 1 } else { context::repo_rating(&app.db, &path).await };
                let (trusted, risk) = match context::attach_rating(&app.db, id, &text, in_trusted, repo).await {
                    Ok(r) => r,
                    Err(e) => {
                        out.content.push_str(&format!("\n\n[instructions in {shown}: not shown, their origin couldn't be rated ({e:#})]"));
                        continue;
                    }
                };
                if !trusted {
                    if let Some(r) = out.rating.as_mut() { r.risk = r.risk.max(risk.min(4)); r.origin.push_str("+untrusted-context"); }
                    // A local project with no remote is the owner's own: its instructions are
                    // shown as content but don't raise the session.
                    if risk <= 1 {
                        if let Err(e) = tape::append(&app.db, id, "context", &json!({ "path": path, "trusted": false, "risk": risk })).await {
                            tracing::error!("recording instruction file {} for {id}: {e:#}", path.display());
                        }
                        let text = context::capped(context::label(&shown, risk, &text));
                        out.content.push_str(&context::untrusted_attachment(&path, &text));
                        continue;
                    }
                    if let Err(e) = crate::taint::mark_rating(app, id, risk, "project file", &shown).await {
                        out.content.push_str(&format!("\n\n[instructions in {shown}: {}]", crate::taint::withheld(&e)));
                        continue;
                    }
                }
                if let Err(e) = tape::append(&app.db, id, "context", &json!({ "path": path, "trusted": trusted, "risk": risk })).await {
                    tracing::error!("recording instruction file {} for {id}: {e:#}", path.display());
                }
                let text = context::capped(context::label(&shown, risk, &text));
                out.content.push_str(&if trusted { context::attachment(&path, &text) } else { context::untrusted_attachment(&path, &text) });
            }
            // A source can be denied while a read is in flight. Dependencies are recorded before
            // handover, so recheck the live graph before returning any result to the worker.
            if crate::taint::stopped(&app.db,id).await.unwrap_or(true) {
                out.content = "result withheld: session or a source was contaminated during this tool".into();
                out.is_error = true;
            }
            // Every tool's output, instruction files included, is masked before the model or the tape sees it.
            out.content = crate::secrets::mask_off_thread(std::mem::take(&mut out.content)).await;
            // Record provenance before giving the worker content. Failure withholds the result.
            let rating = match out.rating.take() {
                Some(r) => r,
                None => crate::sources::Rating {
                    risk: sqlx::query_scalar("SELECT risk FROM sessions WHERE id=$1").bind(id).fetch_one(&app.db).await?,
                    origin: format!("kernel:{name}"),
                },
            };
            if rating.risk < 5 {
                if matches!(name.as_str(), "web_search" | "web_fetch") && rating.origin.starts_with("web_") {
                    // Numeric web provenance must also preserve the level-4 review/board marker.
                    // Do not mark failed/held calls merely because their inherited session risk is 4.
                    let about = args[if name == "web_search" { "query" } else { "url" }].as_str().unwrap_or("");
                    crate::taint::mark_rating(app,id,rating.risk,&name,about).await?;
                } else {
                    crate::sources::raise(&app.db, id, rating.risk).await?;
                }
            }
            if rating.risk == 5 {
                crate::taint::deny(app,id,"level-5 block").await?;
                out.content = "content withheld: level-5 block stopped this session; owner review required".into();
                out.is_error = true;
            }
            let output_bytes = out.content.len() as i64;
            // Cut arguments keep the sha256 and size of the whole (masked) arguments under `_full`.
            let recorded = sqlx::query(
                "INSERT INTO tool_calls (session_id, call_id, name, args, is_error, duration_ms, output_bytes, turn_id, risk, origin, result_sha256, source_args)
                 VALUES ($1, $2, $3, CASE WHEN $9::text IS NULL THEN $4 ELSE $4 || jsonb_build_object('_full', jsonb_build_object(
                         'sha256', encode(sha256(convert_to($9, 'UTF8')), 'hex'), 'bytes', octet_length($9))) END, $5, $6, $7, $8, $10, $11, encode(sha256(convert_to($12,'UTF8')),'hex'), $13)",
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
            .bind(rating.risk)
            .bind(&rating.origin)
            .bind(&out.content)
            .bind(json!({"query":args["query"].as_str().map(crate::secrets::mask), "focus":args["focus"].as_str().map(crate::secrets::mask)}))
            .execute(&app.db)
            .await;
            if let Err(e) = recorded {
                tracing::error!("recording tool call {call_id} for {id}: {e:#}");
                out.content = "result withheld: provenance could not be recorded".into();
                out.is_error = true;
            }
            if let Some(req_id) = msg.id {
                mind.respond(req_id, &ToolResult { content: out.content, is_error: out.is_error }).await?;
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
            // The session stays on the model it failed over to until the owner picks another
            // (D-058): its next turns, tools' ZEN_MODEL and summaries use it, and clients show it.
            if to != "unknown" {
                if let Some(t) = app.turns.lock().await.get_mut(&id) {
                    t.model = to.clone();
                }
                let effort: Option<String> = sqlx::query_scalar("SELECT effort FROM sessions WHERE id = $1").bind(id).fetch_one(&app.db).await?;
                let effort = effort_on(app, &to, effort).await;
                sqlx::query("UPDATE sessions SET model = $2, effort = $3, updated_at = now() WHERE id = $1")
                    .bind(id)
                    .bind(&to)
                    .bind(&effort)
                    .execute(&app.db)
                    .await?;
                app.emit(id, json!({ "type": "model", "model": to, "effort": effort })).await;
            }
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
