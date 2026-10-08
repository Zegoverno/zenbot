//! A turn's lifecycle: starting it (owner or kernel), compiling what it sends (compile.rs),
//! ending it, recording it (`turns`), and the watchdog for turns that stall.

use super::*;

/// A turn in progress. A session has at most one.
pub(crate) struct Turn {
    pub(crate) worker: usize,
    /// Set to true to stop the kernel's tool calls for this turn (abort, crash, end).
    pub(crate) cancel: watch::Sender<bool>,
    pub(crate) last_activity: Instant,
    pub(crate) tools_running: usize,
    /// When `turn.abort` was sent (by the user or the watchdog); the turn is ended by force if it lingers.
    pub(crate) abort_sent: Option<Instant>,
    pub(crate) model: String,
    /// Instruction files this session has already been given (see context.rs).
    pub(crate) context: HashSet<PathBuf>,
    /// The turn's row in `turns`; its model and tool calls are recorded under it.
    pub(crate) turn_id: Uuid,
    pub(crate) started: Instant,
    /// Model id the provider reported for the turn's latest model call.
    pub(crate) model_resolved: Option<String>,
    /// The worker's `turn.usage` report, if any (engine, version, turn totals).
    pub(crate) reported: Value,
    /// Where this session's tools run: its own workspace, or the kernel's.
    pub(crate) workspace: PathBuf,
    /// What the turn sent (measure::record) and why it can't reuse the cache, judged at the start.
    pub(crate) sent: Value,
    pub(crate) cache_break: Option<&'static str>,
    /// The previous turn's context size, to check the provider really reused its cache.
    pub(crate) prev_context: Option<i64>,
    /// Set when a tool ended the model's turn (a question to the owner, a verdict; agent.rs);
    /// later tool calls are refused.
    pub(crate) ending: Option<&'static str>,
    /// The session's kind (`verifier` for a child that checks work; agent.rs), read when the turn
    /// starts, so tool calls don't read it from the database.
    pub(crate) kind: Option<String>,
}

/// Stop turns that have gone quiet: no message from the worker and no tool running for
/// ZEN_TURN_IDLE_SECS (default 600). First ask the worker to abort; if the turn is still
/// there ZEN_TURN_ABORT_GRACE_SECS (default 30) after any abort, end it in the kernel.
pub(crate) async fn watchdog(app: AppState) {
    let idle_limit = Duration::from_secs(crate::env_num("ZEN_TURN_IDLE_SECS", 600.0) as u64);
    let grace = Duration::from_secs(crate::env_num("ZEN_TURN_ABORT_GRACE_SECS", 30.0) as u64);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut to_abort = Vec::new();
        let mut to_end = Vec::new();
        for (id, t) in app.turns.lock().await.iter_mut() {
            match t.abort_sent {
                Some(at) if at.elapsed() > grace => to_end.push(*id),
                None if t.tools_running == 0 && t.last_activity.elapsed() > idle_limit => {
                    t.abort_sent = Some(Instant::now());
                    let _ = t.cancel.send(true);
                    to_abort.push((*id, t.worker));
                }
                _ => {}
            }
        }
        for (id, worker) in to_abort {
            tracing::warn!("turn in session {id} stalled; aborting");
            let _ = app.workers[worker].mind().request("turn.abort", json!({ "session_id": id })).await;
        }
        for id in to_end {
            tracing::warn!("turn in session {id} did not stop after abort; ending it");
            finish_turn(&app, id, json!("the turn stopped responding and was ended")).await;
        }
    }
}

/// End a session's turn in the kernel: cancel its tool calls, free the session, tell clients.
/// Returns false if no turn was running (e.g. it was already ended).
pub(crate) async fn finish_turn(app: &AppState, id: Uuid, error: Value) -> bool {
    let Some(turn) = app.turns.lock().await.remove(&id) else { return false };
    let _ = turn.cancel.send(true);
    let Recorded { turn: summary, parent, session_cost: cost } = match record_turn(app, &turn, &error).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("recording turn {}: {e:#}", turn.turn_id);
            let parent = sqlx::query_scalar("SELECT parent FROM sessions WHERE id = $1").bind(id).fetch_optional(&app.db).await.ok().flatten().flatten();
            let cost = sqlx::query_scalar(&format!("SELECT {}", session_cost("$1"))).bind(id).fetch_one(&app.db).await.unwrap_or(0.0);
            Recorded { turn: Value::Null, parent, session_cost: cost }
        }
    };
    // A cleanly finished turn leaves the engine's own session (if it keeps one) in sync with the tape.
    let es = &turn.reported["engine_session"];
    if error.is_null() && es["resumable"] == true {
        let engine = turn.model.split_once('/').map(|(e, _)| e).unwrap_or("");
        if let Err(e) = tape::append(&app.db, id, "engine_session", &json!({ "engine": engine, "id": es["id"] })).await {
            tracing::error!("recording engine session for {id}: {e:#}");
        }
    }
    app.emit(id, json!({ "type": "end", "error": error, "cost": cost, "turn": summary, "next": false })).await;
    if let Some(waiter) = app.waiters.lock().await.remove(&id) {
        let _ = waiter.send(());
    }
    // A child session's turn (a verifier) counts toward its parent's work: tell the parent's clients.
    if let Some(parent) = parent {
        app.emit(parent, json!({ "type": "child_end", "turn": summary })).await;
    }
    if error.is_null() {
        let size = summary["context_tokens"].as_i64().or(turn.sent["est_tokens"].as_i64()).unwrap_or(0);
        prepare_summary_if_needed(app, id, &turn.model, size).await;
    }
    true
}

/// Past the soft limit, prepare a summary in the background so the next turn after a pause can
/// apply it (compact.rs). One at a time per session; none while one is waiting to be applied.
pub(crate) async fn prepare_summary_if_needed(app: &AppState, id: Uuid, model: &str, size: i64) {
    let info = model_info(app, model).await.unwrap_or(Value::Null);
    let settings = compact::Settings::from_env(info["context"].as_i64());
    if !settings.over_soft(size) || !app.compacting.lock().await.insert(id) {
        return;
    }
    let (app, model) = (app.clone(), model.to_string());
    tokio::spawn(async move {
        let res = match compact::pending(&app.db, id).await {
            Ok(Some(_)) => Ok(None),
            Ok(None) => compact::prepare(&app, id, settings.keep_tokens(), &model).await,
            Err(e) => Err(e.into()),
        };
        match res {
            Ok(Some(c)) => tracing::info!("prepared summary {c} for session {id} ({size} tokens)"),
            Ok(None) => {}
            Err(e) => tracing::error!("preparing a summary for session {id}: {e:#}"),
        }
        app.compacting.lock().await.remove(&id);
    });
}

/// A session's cost: its turns, plus calls recorded before turns were (they have no turn_id).
pub(crate) fn session_cost(session: &str) -> String {
    format!(
        "(COALESCE((SELECT SUM(cost_usd) FROM turns t WHERE t.session_id = {session}), 0)
          + COALESCE((SELECT SUM(cost_usd) FROM model_calls m WHERE m.session_id = {session} AND m.turn_id IS NULL), 0))"
    )
}

/// A closed turn's row as JSON, with what ending it needs from its session.
pub(crate) struct Recorded {
    pub(crate) turn: Value,
    /// The session's parent (a verifier's), whose clients hear of the turn.
    pub(crate) parent: Option<Uuid>,
    /// The session's cost, this turn included.
    pub(crate) session_cost: f64,
}

/// Close a turn's row: outcome, duration, counts and totals. The worker's own turn report wins
/// where it has a figure (Claude Code's covers side calls the stream never shows); otherwise the
/// totals are summed from the turn's model calls. Returns the row, its session's parent and cost.
pub(crate) async fn record_turn(app: &App, turn: &Turn, error: &Value) -> Result<Recorded> {
    let outcome = match error.as_str() {
        None => "ok",
        Some("interrupted") => "interrupted",
        Some(_) => "error",
    };
    let r = &per_turn_report(app, turn).await?;
    let int = |k: &str| r.get(k).and_then(Value::as_i64);
    let (first_read, context_tokens) = measure::provider_numbers(&app.db, turn.turn_id).await?;
    let render = turn.reported["render"].as_str();
    let cache_break = measure::break_at_end(turn.cache_break, render, turn.sent["resume_offered"] == true, first_read, turn.prev_context);
    let mut sent = turn.sent.clone();
    if !sent.is_null() {
        sent["render"] = json!(render);
        sent["first_cache_read"] = json!(first_read);
    }
    let row = sqlx::query(
        "WITH calls AS (SELECT COUNT(*)::int AS n, SUM(input_tokens) AS i, SUM(output_tokens) AS o, SUM(cache_read) AS cr,
                               SUM(cache_write) AS cw, SUM(cost_usd) AS c FROM model_calls WHERE turn_id = $1),
              tools AS (SELECT COUNT(*)::int AS n, (COUNT(*) FILTER (WHERE is_error))::int AS e FROM tool_calls WHERE turn_id = $1)
         UPDATE turns SET ended_at = now(), duration_ms = $2, outcome = $3, error = $4,
                engine = $5, engine_version = $6, model_resolved = $7,
                model_calls = (SELECT n FROM calls), tool_calls = (SELECT n FROM tools), tool_errors = (SELECT e FROM tools),
                input_tokens = COALESCE($8, (SELECT i FROM calls), 0), output_tokens = COALESCE($9, (SELECT o FROM calls), 0),
                cache_read = COALESCE($10, (SELECT cr FROM calls), 0), cache_write = COALESCE($11, (SELECT cw FROM calls), 0),
                cost_usd = COALESCE($12, (SELECT c FROM calls), 0), usage = $13,
                cache_break = $14, context_tokens = $15, context = COALESCE($16, context)
         WHERE id = $1
         RETURNING id, harness, worker, engine, engine_version, model, model_resolved, effort, duration_ms, outcome,
                   model_calls, tool_calls, tool_errors, input_tokens, output_tokens, cache_read, cache_write, cost_usd,
                   cache_break, context_tokens, context->>'render' AS render,
                   (SELECT parent FROM sessions WHERE id = turns.session_id) AS parent,
                   -- The session's cost: subqueries see the row before this update, so this
                   -- turn's new cost is added to the other turns'.
                   COALESCE((SELECT SUM(t.cost_usd) FROM turns t WHERE t.session_id = turns.session_id AND t.id <> turns.id), 0)
                     + COALESCE(cost_usd, 0)
                     + COALESCE((SELECT SUM(m.cost_usd) FROM model_calls m WHERE m.session_id = turns.session_id AND m.turn_id IS NULL), 0)
                     AS session_cost",
    )
    .bind(turn.turn_id)
    .bind(turn.started.elapsed().as_millis() as i64)
    .bind(outcome)
    .bind(error.as_str())
    .bind(r["engine"].as_str())
    .bind(r["engine_version"].as_str())
    .bind(&turn.model_resolved)
    .bind(int("input"))
    .bind(int("output"))
    .bind(int("cache_read"))
    .bind(int("cache_write"))
    .bind(r.get("cost_usd").and_then(Value::as_f64))
    .bind(if turn.reported.is_null() { None } else { Some(&turn.reported) })
    .bind(&cache_break)
    .bind(context_tokens)
    .bind(if sent.is_null() { None } else { Some(&sent) })
    .fetch_one(&app.db)
    .await?;
    Ok(Recorded { turn: turn_json(&row), parent: row.get("parent"), session_cost: row.get("session_cost") })
}

/// The worker's report with its totals made per turn. An engine that continues its own session
/// (Claude Code with --resume) reports totals for the whole session so far; the previous turn's
/// report for the same engine session is subtracted. The raw report is stored as it came.
pub(crate) async fn per_turn_report(app: &App, turn: &Turn) -> Result<Value> {
    let mut r = turn.reported.clone();
    if r["render"] != "resume" {
        return Ok(r);
    }
    let previous: Option<Value> = sqlx::query_scalar(
        "SELECT usage FROM turns WHERE session_id = (SELECT session_id FROM turns WHERE id = $1)
           AND id <> $1 AND usage->'engine_session'->>'id' = $2 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(turn.turn_id)
    .bind(r["engine_session"]["id"].as_str().unwrap_or(""))
    .fetch_optional(&app.db)
    .await?
    .flatten();
    let Some(prev) = previous else { return Ok(r) };
    for k in ["input", "output", "cache_read", "cache_write"] {
        if let (Some(now), Some(before)) = (r[k].as_i64(), prev[k].as_i64()) {
            r[k] = json!((now - before).max(0));
        }
    }
    if let (Some(now), Some(before)) = (r["cost_usd"].as_f64(), prev["cost_usd"].as_f64()) {
        r["cost_usd"] = json!((now - before).max(0.0));
    }
    Ok(r)
}

pub(crate) fn turn_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "turn_id": r.get::<Uuid, _>("id"),
        "harness": r.get::<String, _>("harness"),
        "worker": r.get::<String, _>("worker"),
        "engine": r.get::<Option<String>, _>("engine"),
        "engine_version": r.get::<Option<String>, _>("engine_version"),
        "model": r.get::<String, _>("model"),
        "model_resolved": r.get::<Option<String>, _>("model_resolved"),
        "effort": r.get::<Option<String>, _>("effort"),
        "duration_ms": r.get::<Option<i64>, _>("duration_ms"),
        "outcome": r.get::<Option<String>, _>("outcome"),
        "model_calls": r.get::<Option<i32>, _>("model_calls"),
        "tool_calls": r.get::<Option<i32>, _>("tool_calls"),
        "tool_errors": r.get::<Option<i32>, _>("tool_errors"),
        "input_tokens": r.get::<Option<i64>, _>("input_tokens"),
        "output_tokens": r.get::<Option<i64>, _>("output_tokens"),
        "cache_read": r.get::<Option<i64>, _>("cache_read"),
        "cache_write": r.get::<Option<i64>, _>("cache_write"),
        "cost_usd": r.get::<Option<f64>, _>("cost_usd"),
        "cache_break": r.get::<Option<String>, _>("cache_break"),
        "context_tokens": r.get::<Option<i64>, _>("context_tokens"),
        "render": r.get::<Option<String>, _>("render"),
    })
}

/// Who started a turn: the owner (a prompt), or the kernel (a verifier's review).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Origin {
    Owner,
    Kernel,
}

/// The owner's prompt.
pub(crate) async fn start_turn(app: &AppState, id: Uuid, text: String) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    begin_turn(app, id, text, Origin::Owner).await
}

/// Run a child session of `kind` (a verifier, a subagent) with one kernel prompt, on `model` (the
/// parent's when None), and wait for it to end (30 minutes at most). It inherits the parent's
/// taint. Returns the child's id; the caller reads what it needs from its tape.
pub(crate) async fn run_child(app: &AppState, parent: Uuid, kind: &str, prompt: &str, dir: &std::path::Path, model: Option<&str>) -> Result<Uuid> {
    let child = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, title, model, effort, state, parent, kind, workspace, tainted_at)
         SELECT $1, $2 || ': ' || title, COALESCE($6, model), CASE WHEN $6 IS NULL THEN effort END, NULL, id, $3, $5, tainted_at
         FROM sessions WHERE id = $4",
    )
    .bind(child)
    .bind(kind)
    .bind(kind)
    .bind(parent)
    .bind(dir.display().to_string())
    .bind(model)
    .execute(&app.db)
    .await?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.waiters.lock().await.insert(child, tx);
    if let Err(e) = begin_turn(app, child, prompt.to_string(), Origin::Kernel).await {
        app.waiters.lock().await.remove(&child);
        return Err(e);
    }
    if tokio::time::timeout(Duration::from_secs(1800), rx).await.is_err() {
        tracing::warn!("{kind} session {child} took over 30 minutes; stopping it");
        abort_turn(app, child).await;
    }
    app.waiters.lock().await.remove(&child);
    Ok(child)
}

pub(crate) async fn begin_turn(app: &AppState, id: Uuid, text: String, origin: Origin) -> Result<()> {
    let row = sqlx::query("SELECT model, effort, title, workspace, kind FROM sessions WHERE id = $1")
        .bind(id)
        .fetch_optional(&app.db)
        .await?;
    let Some(row) = row else { anyhow::bail!("session not found") };
    let model: String = row.get("model");
    let workspace = row.get::<Option<String>, _>("workspace").map(PathBuf::from).unwrap_or_else(|| app.workspace.clone());
    let kind: Option<String> = row.get("kind");
    let title: String = row.get("title");

    let Some(widx) = worker_for(app, &model).await else {
        anyhow::bail!("no worker serves model `{model}`; pick another with /model");
    };
    // Always send an explicit level, so what ran is known (engines don't all report their default).
    let info = model_info(app, &model).await.unwrap_or(Value::Null);
    let effort: Option<String> = row.get::<Option<String>, _>("effort").or_else(|| info["default_effort"].as_str().map(String::from));
    if let Some(e) = &effort {
        if !efforts(&info).contains(&e.as_str()) {
            anyhow::bail!("model `{model}` doesn't take effort `{e}`; pick another with /effort");
        }
    }
    let turn_id = Uuid::new_v4();
    let (cancel, mut stopped) = watch::channel(false);
    {
        let mut turns = app.turns.lock().await;
        if turns.contains_key(&id) {
            anyhow::bail!("this session is already working; wait or abort");
        }
        turns.insert(
            id,
            Turn {
                worker: widx,
                cancel,
                last_activity: Instant::now(),
                tools_running: 0,
                abort_sent: None,
                model: model.clone(),
                context: HashSet::new(),
                turn_id,
                started: Instant::now(),
                model_resolved: None,
                reported: Value::Null,
                workspace: workspace.clone(),
                sent: Value::Null,
                cache_break: None,
                prev_context: None,
                ending: None,
                kind: kind.clone(),
            },
        );
    }
    // How far the start got, for cleaning up when it fails.
    let (mut row_written, mut busy_sent) = (false, false);
    let res: Result<()> = async {
        let prev = measure::previous(&app.db, id).await?;
        // A prepared summary is applied after a pause (the cache has expired anyway) or past the
        // hard limit; past the hard limit with none prepared, one is made now.
        if let Some(p) = &prev {
            let settings = compact::Settings::from_env(info["context"].as_i64());
            let paused = p.idle_secs.is_some_and(|s| s >= settings.idle_secs);
            let over = settings.over_hard(p.size());
            let ready = match compact::pending(&app.db, id).await? {
                Some(c) => Some(c),
                None if over => {
                    app.emit(id, json!({ "type": "status", "text": "summarizing older turns to make room" })).await;
                    // The summary can take minutes: the watchdog must not take that for a stalled
                    // turn, and an abort stops the wait.
                    hold(app, id, turn_id, true).await;
                    let made = tokio::select! {
                        made = compact::prepare(app, id, settings.keep_tokens(), &model) => Some(made),
                        _ = stopped.wait_for(|s| *s) => None,
                    };
                    hold(app, id, turn_id, false).await;
                    made.unwrap_or_else(|| Err(anyhow::anyhow!(INTERRUPTED)))?
                }
                None => None,
            };
            if let Some(c) = ready.filter(|_| paused || over) {
                compact::apply(&app.db, id, c).await?;
                tracing::info!("applied summary {c} to session {id}");
            }
        }
        // The engine's own session can be resumed only if nothing was written since it last
        // matched the tape (no other turn, summary or new instructions in between).
        // Bookkeeping blocks (questions, verifications, …) don't touch what the engine saw.
        // The tape is read once; everything below is derived from it.
        let blocks = tape::load_all(&app.db, id).await?;
        let engine = model.split_once('/').map(|(e, _)| e).unwrap_or("");
        let last = blocks.iter().rev().find(|b| matches!(b.kind.as_str(), "message" | "compaction" | "envelope" | "base" | "engine_session")).cloned();
        let base = compile::base_prompt(&app.db, id, &blocks, &app.workspace, &app.repo).await?;
        let tools = agent::specs(kind.as_deref());
        let system = agent::system_for(&base, kind.as_deref());
        let (envelope, new_envelope) = compile::envelope(&app.db, id, &blocks, &system, &tools).await?;
        let resume = match (&last, new_envelope) {
            (Some(b), None) if b.kind == "engine_session" && b.payload["engine"] == engine => Some(json!({ "id": b.payload["id"] })),
            _ => None,
        };
        let (history, summary) = compile::history(&blocks);
        let today = chrono::Local::now().format("%Y-%m-%d (%A)").to_string();
        let turn_context = compile::turn_context(&blocks, &today);
        let sent = measure::record(&envelope, &history, &summary, &text, &turn_context, resume.is_some(), new_envelope);
        let cache_break = measure::break_at_start(prev.as_ref(), &model, &envelope.hash, &sent);
        // An abort or the watchdog may have ended the turn while it was being prepared: then
        // finish_turn has told the clients, and there's nothing left to do.
        if !still_ours(app, id, turn_id).await? {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO turns (id, session_id, harness, worker, model, effort, envelope, context, cache_break)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(turn_id)
        .bind(id)
        .bind(&app.harness)
        .bind(&app.workers[widx].name)
        .bind(&model)
        .bind(&effort)
        .bind(&envelope.hash)
        .bind(&sent)
        .bind(cache_break)
        .execute(&app.db)
        .await?;
        row_written = true;
        let extra = context_in(&blocks);
        if let Some(t) = app.turns.lock().await.get_mut(&id) {
            t.context = extra.iter().cloned().collect();
            t.sent = sent;
            t.cache_break = cache_break;
            t.prev_context = prev.as_ref().and_then(|p| p.context_tokens);
        }
        let mut user = json!({ "role": "user", "content": text, "timestamp": chrono::Utc::now().timestamp_millis() });
        if let Some(c) = &turn_context {
            user["context"] = json!(c);
        }
        if origin == Origin::Kernel {
            user["kernel"] = json!(true);
        }
        tape::append(&app.db, id, "message", &user).await?;
        if title.is_empty() && origin == Origin::Owner {
            let t: String = text.chars().take(60).collect();
            sqlx::query("UPDATE sessions SET title = $2 WHERE id = $1").bind(id).bind(t.trim()).execute(&app.db).await?;
        }
        app.emit(id, json!({ "type": "message", "message": user })).await;
        if !still_ours(app, id, turn_id).await? {
            return Ok(());
        }
        // `busy` goes out before the worker starts, so it can't arrive after the turn's `end`.
        app.emit(id, json!({ "type": "busy", "busy": true, "turn_id": turn_id, "harness": app.harness, "model": model, "effort": effort })).await;
        busy_sent = true;
        app.workers[widx]
            .mind()
            .request(
                "turn.start",
                json!({
                    "session_id": id,
                    "turn_id": turn_id,
                    "model": model,
                    "effort": effort,
                    "system_prompt": envelope.system,
                    "history": history,
                    "prompt": text,
                    "prompt_context": turn_context,
                    "tools": envelope.tools,
                    "resume": resume,
                    "kind": kind,
                }),
            )
            .await?;
        Ok(())
    }
    .await;
    match res {
        Ok(()) => Ok(()),
        Err(e) => {
            // The turn never started; close its row (if it was written) as an error, and end it
            // for clients that were told it started.
            let turn = {
                let mut turns = app.turns.lock().await;
                if turns.get(&id).is_some_and(|t| t.turn_id == turn_id) {
                    turns.remove(&id)
                } else {
                    None
                }
            };
            if let Some(turn) = turn {
                let error = json!(e.to_string());
                let summary = if row_written { record_turn(app, &turn, &error).await.map(|r| r.turn).unwrap_or(Value::Null) } else { Value::Null };
                if busy_sent {
                    app.emit(id, json!({ "type": "end", "error": error, "turn": summary, "next": false })).await;
                }
            }
            Err(e)
        }
    }
}

/// The error a turn stopped before it reached the worker ends with (an abort while summarizing).
const INTERRUPTED: &str = "interrupted";

/// Whether the session's turn is still the one `turn_id` names and hasn't been stopped. Errors
/// (as interrupted) when it is but was stopped; false when it is gone (ended by the kernel).
async fn still_ours(app: &App, id: Uuid, turn_id: Uuid) -> Result<bool> {
    match app.turns.lock().await.get(&id) {
        Some(t) if t.turn_id == turn_id => {
            anyhow::ensure!(!*t.cancel.borrow(), INTERRUPTED);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Count kernel work for a turn before it reaches the worker (a summary made inline) as a running
/// tool, so the watchdog doesn't take the quiet for a stall; it counts as activity when it ends.
async fn hold(app: &App, id: Uuid, turn_id: Uuid, on: bool) {
    if let Some(t) = app.turns.lock().await.get_mut(&id).filter(|t| t.turn_id == turn_id) {
        if on {
            t.tools_running += 1;
        } else {
            t.tools_running = t.tools_running.saturating_sub(1);
            t.last_activity = Instant::now();
        }
    }
}

/// Stop a session's turn: its tools at once, then the model (the worker's `turn.abort`). The
/// watchdog ends the turn if it lingers.
pub(crate) async fn abort_turn(app: &App, id: Uuid) {
    let worker = app.turns.lock().await.get_mut(&id).map(|t| {
        let _ = t.cancel.send(true);
        t.abort_sent.get_or_insert_with(Instant::now);
        t.worker
    });
    if let Some(w) = worker.and_then(|i| app.workers.get(i)) {
        let _ = w.mind().request("turn.abort", json!({ "session_id": id })).await;
    }
}
