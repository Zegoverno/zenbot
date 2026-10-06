//! Messages from workers: tool calls (run by the kernel, gated by the workflow), streamed text,
//! finished messages, usage reports and turn ends.

use super::*;

pub(crate) async fn dispatch(app: AppState, mut incoming: tokio::sync::mpsc::UnboundedReceiver<(usize, Incoming)>) {
    // Notifications are handled in arrival order so streams and the tape stay ordered.
    // Tool calls run concurrently; the worker already emitted the assistant message that requested them.
    while let Some((worker, msg)) = incoming.recv().await {
        if msg.method == "tool.call" {
            let app = app.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_incoming(&app, worker, msg).await {
                    tracing::error!("handling tool call: {e:#}");
                }
            });
        } else if let Err(e) = handle_incoming(&app, worker, msg).await {
            tracing::error!("handling mind message: {e:#}");
        }
    }
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

pub(crate) async fn handle_incoming(app: &AppState, worker: usize, msg: Incoming) -> Result<()> {
    let mind = app.workers[worker].mind();
    let p = &msg.params;
    let id = session_id(p);
    if let Ok(id) = id {
        if !touch(app, id, worker, turn_id(p)).await {
            tracing::warn!("dropping `{}` for session {id}: not from the turn running on that worker", msg.method);
            if let Some(req_id) = msg.id {
                mind.respond(req_id, json!({ "content": "this turn has ended", "is_error": true })).await?;
            }
            return Ok(());
        }
    }
    match msg.method.as_str() {
        "tool.call" => {
            let id = id?;
            let name = p.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let call_id = p.get("call_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let args = p.get("args").cloned().unwrap_or(json!({}));
            let (mut cancel, model, turn_id, mut ending, workspace) = {
                let mut turns = app.turns.lock().await;
                let Some(t) = turns.get_mut(&id) else { return Ok(()) };
                t.tools_running += 1;
                (t.cancel.subscribe(), t.model.clone(), t.turn_id, t.ending, t.workspace.clone())
            };
            let state = flow::state(&app.db, id).await.unwrap_or_else(|_| "open".into());
            // Commands can tell which session runs them (e.g. the commit-trailer hook in scripts/git-hooks).
            let session_env = id.to_string();
            let env = [("ZEN_SESSION_ID", session_env.as_str()), ("ZEN_MODEL", model.as_str())];
            app.emit(id, json!({ "type": "tool_start", "call_id": call_id, "name": name, "args": args })).await;
            let started = Instant::now();
            // Abort (or the turn ending) drops the tool future, which kills a running command.
            let mut out = if *cancel.borrow() {
                tools::ToolOutput { content: "not run: the turn was interrupted".into(), is_error: true }
            } else {
                tokio::select! {
                    out = async {
                        // The workflow decides what may run in this state (flow.rs); then its own
                        // tools, the history tool, or the kernel's built-ins.
                        let refused = if flow::enabled() { flow::refuse(&state, &name, ending) } else { None };
                        if let Some(why) = refused {
                            tools::ToolOutput { content: why, is_error: true }
                        } else if let Some(out) = flow::run_tool(app, id, &name, &args, &mut ending).await {
                            out
                        } else if name == "history" {
                            let (content, is_error) = compact::history_tool(&app.db, id, &args).await;
                            tools::ToolOutput { content, is_error }
                        } else {
                            tools::execute(&workspace, &name, &args, &env, flow::read_only(&state)).await
                        }
                    } => out,
                    _ = cancel.wait_for(|c| *c) => tools::ToolOutput { content: "interrupted: the turn was stopped before this tool finished".into(), is_error: true },
                }
            };
            let ms = started.elapsed().as_millis() as i64;
            // First time this session touches a project with its own instructions: attach them.
            let new_context: Vec<PathBuf> = {
                let candidates = context::governing(&workspace, &context::paths_in_call(&workspace, &name, &args));
                let mut turns = app.turns.lock().await;
                match turns.get_mut(&id) {
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
            for path in new_context {
                if let Some(text) = context::read_capped(&path) {
                    if let Err(e) = append_tape(&app.db, id, "context", &json!({ "path": path })).await {
                        tracing::error!("recording instruction file {} for {id}: {e:#}", path.display());
                    }
                    out.content.push_str(&context::attachment(&path, &text));
                }
            }
            // Answer the worker first: a recording failure must not leave the model waiting.
            if let Some(req_id) = msg.id {
                mind.respond(req_id, json!({ "content": out.content, "is_error": out.is_error })).await?;
            }
            let recorded = sqlx::query(
                "INSERT INTO tool_calls (session_id, call_id, name, args, is_error, duration_ms, output_bytes, turn_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(id)
            .bind(&call_id)
            .bind(&name)
            .bind(&args)
            .bind(out.is_error)
            .bind(ms)
            .bind(out.content.len() as i64)
            .bind(turn_id)
            .execute(&app.db)
            .await;
            if let Err(e) = recorded {
                tracing::error!("recording tool call {call_id} for {id}: {e:#}");
            }
            app.emit(id, json!({ "type": "tool_end", "call_id": call_id, "is_error": out.is_error, "ms": ms })).await;
        }
        "turn.delta" | "turn.thinking" => {
            let id = id?;
            let kind = if msg.method == "turn.delta" { "delta" } else { "thinking" };
            app.emit(id, json!({ "type": kind, "delta": p.get("delta") })).await;
        }
        "turn.message" => {
            let id = id?;
            let message = p.get("message").cloned().unwrap_or(Value::Null);
            append_tape(&app.db, id, "message", &message).await?;
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
        "turn.usage" => {
            // The worker's report for the whole turn (engine, version, totals); recorded when the turn ends.
            let id = id?;
            if let Some(t) = app.turns.lock().await.get_mut(&id) {
                t.reported = p.clone();
            }
        }
        "turn.end" => {
            finish_turn(app, id?, p.get("error").cloned().unwrap_or(Value::Null)).await;
        }
        other => {
            tracing::warn!("unknown method from worker: {other}");
            if let Some(req_id) = msg.id {
                mind.respond(req_id, json!({ "content": format!("unknown method {other}"), "is_error": true })).await?;
            }
        }
    }
    Ok(())
}
