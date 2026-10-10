//! The HTTP API and the WebSocket for clients (docs/client-protocol.md).

use super::*;

pub(crate) struct ApiError(pub(crate) StatusCode, pub(crate) String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl<E: std::fmt::Display> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;

pub(crate) fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "session not found".into())
}

/// Whether two byte strings are equal, in time that depends only on their lengths (so a wrong token
/// can't be guessed byte by byte from response times).
pub(crate) fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) async fn auth(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let token = app.token.as_bytes();
    let header_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| same_secret(v.strip_prefix("Bearer ").unwrap_or(v).as_bytes(), token));
    // Only the header: a token in the URL ends up in logs and history.
    if header_ok {
        next.run(req).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "unauthorized".into()).into_response()
    }
}

pub(crate) async fn health(State(app): State<AppState>) -> Json<Value> {
    let db = sqlx::query("SELECT 1").execute(&app.db).await.is_ok();
    let mut workers = serde_json::Map::new();
    for w in &app.workers {
        // Unauthenticated and polled by upgrades: a hung worker must not hold it for 30 s.
        let ping = w.mind().request_within("ping", json!({}), Duration::from_secs(3)).await.is_ok();
        workers.insert(w.name.clone(), json!(ping));
    }
    let mind = workers.values().all(|v| v == true);
    let busy = app.turns.lock().await.len() + app.background.load(std::sync::atomic::Ordering::SeqCst);
    Json(json!({ "ok": db && mind, "db": db, "mind": mind, "workers": workers, "busy": busy,
                 "version": env!("CARGO_PKG_VERSION"), "commit": app.updater.running() }))
}

#[derive(Deserialize)]
pub(crate) struct VersionQuery {
    refresh: Option<bool>,
}

/// Running version and whether origin/main has newer commits (`?refresh=true` checks now).
pub(crate) async fn version(State(app): State<AppState>, Query(q): Query<VersionQuery>) -> Json<Value> {
    Json(if q.refresh.unwrap_or(false) { app.updater.check().await } else { app.updater.info().await })
}

/// Pull the latest main and apply it (scripts/self-update.sh); progress via GET /api/upgrade.
pub(crate) async fn upgrade_start(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    match app.updater.start().await {
        Ok(started_at) => Ok(Json(json!({ "started_at": started_at }))),
        Err(e) => Err(ApiError(StatusCode::CONFLICT, e)),
    }
}

pub(crate) async fn upgrade_status(State(app): State<AppState>) -> Json<Value> {
    Json(app.updater.status().await)
}

pub(crate) async fn list_models(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(collect_models(&app).await))
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    archived: Option<bool>,
}

pub(crate) async fn list_sessions(State(app): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(&format!(
        "SELECT {SESSION_COLUMNS}, {} AS cost
         FROM sessions s WHERE s.archived = $1 AND (s.kind IS NULL OR s.kind = 'job') ORDER BY s.updated_at DESC",
        session_cost("s.id")
    ))
    .bind(q.archived.unwrap_or(false))
    .fetch_all(&app.db)
    .await?;
    Ok(Json(Value::Array(rows.iter().map(session_json).collect())))
}

/// Every session for the sessions board (`zen`'s home screen): main and job sessions, archived
/// ones too, and their child sessions (subagents, verifiers) with the task each was given. Each
/// says whether a turn is running in it right now, and whether it's waiting on the owner: its
/// latest `ask` questions came after the owner's latest message (a message the kernel sent in the
/// owner's place, `kernel: true`, isn't an answer). The client nests children under their parent.
/// No costs: the board polls this, and they'd double its time.
pub(crate) async fn board(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(&format!(
        "SELECT {SESSION_COLUMNS},
           CASE WHEN s.parent IS NOT NULL THEN
             (SELECT e.payload->'content' FROM tape_events e
              WHERE e.session_id = s.id AND e.kind = 'message' AND e.payload->>'role' = 'user' ORDER BY e.seq LIMIT 1)
           END AS task,
           q.last IS NOT NULL AND NOT EXISTS (
             SELECT 1 FROM tape_events m
             WHERE m.session_id = s.id AND m.kind = 'message' AND m.seq > q.last
               AND m.payload->>'role' = 'user' AND m.payload->>'kernel' IS NULL) AS waiting
         FROM sessions s
         LEFT JOIN LATERAL (SELECT max(e.seq) AS last FROM tape_events e WHERE e.session_id = s.id AND e.kind = 'questions') q ON true
         ORDER BY s.updated_at DESC"
    ))
    .fetch_all(&app.db)
    .await?;
    let running: HashSet<Uuid> = app.turns.lock().await.keys().copied().collect();
    let sessions = rows
        .iter()
        .map(|r| {
            let mut s = session_json(r);
            // Polled every few seconds: costs (a sum per session) are left to the session's own view.
            s.as_object_mut().map(|o| o.remove("cost"));
            s["busy"] = json!(running.contains(&r.get::<Uuid, _>("id")));
            s["waiting"] = json!(r.get::<Option<bool>, _>("waiting").unwrap_or(false));
            if let Some(task) = r.get::<Option<Value>, _>("task") {
                s["task"] = json!(zen_proto::text_of(&task).chars().take(200).collect::<String>());
            }
            s
        })
        .collect();
    Ok(Json(json!({ "sessions": Value::Array(sessions) })))
}

/// The columns `session_json` reads, without the cost (each query computes it its own way).
const SESSION_COLUMNS: &str = "id, title, model, effort, archived, created_at, updated_at, kind, parent";

pub(crate) fn session_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "title": r.get::<String, _>("title"),
        "model": r.get::<String, _>("model"),
        "effort": r.get::<Option<String>, _>("effort"),
        "archived": r.get::<bool, _>("archived"),
        // None for a session the owner started; "job", "subagent" or "verifier" otherwise.
        "kind": r.get::<Option<String>, _>("kind"),
        "parent": r.get::<Option<Uuid>, _>("parent"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        "cost": r.try_get::<f64, _>("cost").unwrap_or(0.0),
    })
}

#[derive(Deserialize)]
pub(crate) struct CreateSession {
    title: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

pub(crate) async fn create_session(State(app): State<AppState>, Json(body): Json<CreateSession>) -> ApiResult<Json<Value>> {
    let id = Uuid::new_v4();
    let model = body.model.unwrap_or_else(|| app.default_model.clone());
    check_effort(&app, &model, body.effort.as_deref()).await?;
    let row = sqlx::query(&format!(
        "INSERT INTO sessions (id, title, model, effort, title_source) VALUES ($1, $2, $3, $4, CASE WHEN $2 = '' THEN 'auto' ELSE 'owner' END)
         RETURNING {SESSION_COLUMNS}, 0::float8 AS cost"
    ))
    .bind(id)
    .bind(body.title.map(|t| t.trim().to_string()).unwrap_or_default())
    .bind(model)
    .bind(body.effort)
    .fetch_one(&app.db)
    .await?;
    Ok(Json(session_json(&row)))
}

#[derive(Deserialize)]
pub(crate) struct UpdateSession {
    title: Option<String>,
    model: Option<String>,
    /// A thinking level, or "default" for the model's default.
    effort: Option<String>,
    archived: Option<bool>,
}

pub(crate) async fn update_session(State(app): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<UpdateSession>) -> ApiResult<Json<Value>> {
    let current = sqlx::query("SELECT model, effort FROM sessions WHERE id = $1")
        .bind(id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(not_found)?;
    let model = body.model.clone().unwrap_or_else(|| current.get("model"));
    let effort: Option<String> = match body.effort.as_deref() {
        Some("default") => None,
        Some(e) => {
            check_effort(&app, &model, Some(e)).await?;
            Some(e.to_string())
        }
        // A new model keeps the session's effort only if it supports it.
        None => {
            let kept: Option<String> = current.get("effort");
            match kept {
                Some(e) if body.model.is_some() && check_effort(&app, &model, Some(&e)).await.is_err() => None,
                other => other,
            }
        }
    };
    let row = sqlx::query(&format!(
        "UPDATE sessions SET title = COALESCE($2, title), title_source = CASE WHEN $2 IS NULL THEN title_source ELSE 'owner' END, model = COALESCE($3, model), effort = $5,
                archived = COALESCE($4, archived), updated_at = now()
         WHERE id = $1
         RETURNING {SESSION_COLUMNS}, {} AS cost",
        session_cost("$1")
    ))
    .bind(id)
    .bind(body.title)
    .bind(body.model)
    .bind(body.archived)
    .bind(effort)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(not_found)?;
    Ok(Json(session_json(&row)))
}

#[derive(Deserialize)]
pub(crate) struct MessagesQuery {
    /// Only messages after this block number (to catch up after a reconnect).
    after: Option<i32>,
    /// At most this many, the most recent (a long session's tail).
    last: Option<i64>,
}

pub(crate) async fn get_session(State(app): State<AppState>, Path(id): Path<Uuid>, Query(q): Query<MessagesQuery>) -> ApiResult<Json<Value>> {
    let row = sqlx::query(&format!(
        "SELECT {SESSION_COLUMNS}, {} AS cost
         FROM sessions WHERE id = $1",
        session_cost("$1")
    ))
    .bind(id)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(not_found)?;
    let mut session = session_json(&row);
    session["messages"] = Value::Array(match (q.after, q.last) {
        (None, None) => load_messages(&app.db, id).await?,
        (after, last) => {
            // Newest first to apply `last`, then back in order. Uses the (session, kind, seq) index.
            let rows = sqlx::query(
                "SELECT seq, payload FROM tape_events WHERE session_id = $1 AND kind = 'message' AND seq > $2
                 ORDER BY seq DESC LIMIT $3",
            )
            .bind(id)
            .bind(after.unwrap_or(0))
            .bind(last.unwrap_or(i64::MAX).max(0))
            .fetch_all(&app.db)
            .await?;
            rows.iter()
                .rev()
                .map(|r| {
                    let mut m: Value = r.get("payload");
                    m["seq"] = json!(r.get::<i32, _>("seq"));
                    m
                })
                .collect()
        }
    });
    session["busy"] = json!(app.is_busy(id).await);
    Ok(Json(session))
}

/// What the owner decides about the work so far (see migrations/0005).
pub(crate) const DECISIONS: [&str; 4] = ["accept", "more", "reshape", "drop"];

#[derive(Deserialize)]
pub(crate) struct Decide {
    decision: String,
    note: Option<String>,
}

/// Record the owner's decision on the session's work up to its latest turn.
pub(crate) async fn decide(State(app): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<Decide>) -> ApiResult<Json<Value>> {
    if !DECISIONS.contains(&body.decision.as_str()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, format!("decision must be one of {}", DECISIONS.join(", "))));
    }
    let row = sqlx::query(
        "INSERT INTO session_decisions (session_id, turn_id, decision, note)
         SELECT s.id, (SELECT t.id FROM turns t WHERE t.session_id = s.id ORDER BY t.started_at DESC LIMIT 1), $2, $3
         FROM sessions s WHERE s.id = $1
         RETURNING id, turn_id, decision, note, created_at",
    )
    .bind(id)
    .bind(&body.decision)
    .bind(body.note.as_deref().map(str::trim).filter(|n| !n.is_empty()))
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(not_found)?;
    // Accepted work vouches for the draft skills it used (workshop.rs).
    if body.decision == "accept" {
        let app2 = app.clone();
        let work = app.background_work();
        tokio::spawn(async move {
            let _work = work;
            workshop::on_accept(&app2, id).await
        });
    }
    // Score the work the decision covers, so each decision has a score to compare it with.
    let scoring = app.clone();
    let work = app.background_work();
    tokio::spawn(async move {
        let _work = work;
        if let Err(e) = score::score_session(&scoring, id, "decision").await {
            tracing::warn!("scoring session {id} after a decision: {e:#}");
        }
    });
    Ok(Json(json!({
        "id": row.get::<i64, _>("id"),
        "session_id": id,
        "turn_id": row.get::<Option<Uuid>, _>("turn_id"),
        "decision": row.get::<String, _>("decision"),
        "note": row.get::<Option<String>, _>("note"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
    })))
}

pub(crate) async fn session_ws(State(app): State<AppState>, Path(id): Path<Uuid>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(app, id, socket))
}

pub(crate) async fn handle_socket(app: AppState, id: Uuid, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let mut rx = app.subscribe(id).await;
    let fwd_app = app.clone();
    let forward = tokio::spawn(async move {
        loop {
            let msg = match rx.recv().await {
                Ok(msg) => msg,
                // This client fell behind and missed events: say so and carry on. `busy` lets the
                // client finish a turn whose `end` it may have missed.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    json!({ "type": "resync", "skipped": n, "busy": fwd_app.is_busy(id).await }).to_string()
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if sink.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(cmd) = serde_json::from_str::<Value>(&text) else { continue };
        match cmd.get("type").and_then(Value::as_str) {
            Some("prompt") => {
                // Started apart from this loop, so an abort is read while the turn is prepared
                // (a summary at the hard limit can take minutes).
                let text = cmd.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                // What the owner did with the suggested next prompt, if the client showed one.
                let reported = cmd["suggestion"]["id"].as_i64().map(|s| (s, cmd["suggestion"]["taken"] == true));
                let app = app.clone();
                tokio::spawn(async move {
                    if !text.trim().is_empty() && !app.is_busy(id).await {
                        if let Err(e) = assist::settle(&app.db, id, &text, reported).await {
                            tracing::warn!("recording the suggestion's outcome in session {id}: {e:#}");
                        }
                    }
                    if let Err(e) = start_turn(&app, id, text).await {
                        app.emit(id, json!({ "type": "error", "error": e.to_string() })).await;
                    }
                });
            }
            Some("abort") => abort_turn(&app, id).await,
            _ => {}
        }
    }
    forward.abort();
    // Wait until the forwarder (and its receiver) is gone, so the hub sees one client fewer.
    let _ = forward.await;
    app.unsubscribe(id).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_whole() {
        assert!(same_secret(b"zen-token-123", b"zen-token-123"));
        assert!(!same_secret(b"zen-token-123", b"zen-token-124"));
        assert!(!same_secret(b"zen-token-12", b"zen-token-123"));
        assert!(!same_secret(b"", b"x"));
    }
}

#[derive(Deserialize)]
pub(crate) struct MemoryQuery {
    tier: Option<String>,
}

/// Memories (short-term by default; `?tier=archived|long|all`) and the latest sleep.
pub(crate) async fn list_memory(State(app): State<AppState>, Query(q): Query<MemoryQuery>) -> ApiResult<Json<Value>> {
    let tier = q.tier.unwrap_or_else(|| "short".into());
    Ok(Json(json!({
        "memories": memory::list(&app.db, &tier).await?,
        "last_sleep": memory::last_run(&app.db).await?,
        "size": memory::cap(),
    })))
}

#[derive(Deserialize)]
pub(crate) struct Review {
    decision: String,
}

/// `accept` → true, `reject` → false, anything else a 400.
fn accept_or_reject(decision: &str) -> ApiResult<bool> {
    match decision {
        "accept" => Ok(true),
        "reject" => Ok(false),
        _ => Err(ApiError(StatusCode::BAD_REQUEST, "decision must be accept or reject".into())),
    }
}

/// A failed owner action as a 400 with its reason.
fn bad_request(e: anyhow::Error) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
}

#[derive(Deserialize)]
pub(crate) struct SleepQuery {
    trigger: Option<String>,
}

/// Tidy short-term memory now: `?trigger=nightly` is recorded as the nightly sleep (the scheduled
/// `sleep` job calls `memory::sleep` itself), else the owner. The sleep runs in its own task, so a client that stops waiting doesn't cut it short.
pub(crate) async fn run_sleep(State(app): State<AppState>, Query(q): Query<SleepQuery>) -> ApiResult<Json<Value>> {
    let trigger = if q.trigger.as_deref() == Some("nightly") { "nightly" } else { "owner" };
    let job = tokio::spawn(async move { memory::sleep(&app, trigger).await });
    let res = job.await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("sleep task: {e}")))?;
    Ok(Json(res.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?))
}

/// MCP servers from ~/.zenbot/mcp.json: tools per server and problems (connecting the servers).
pub(crate) async fn mcp_status() -> Json<Value> {
    Json(mcp::status().await)
}

/// Skills (active and drafts) with their use, and the tools the agent made.
pub(crate) async fn list_skills(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(workshop::stats(&app.db).await?))
}

#[derive(Deserialize)]
pub(crate) struct SkillReview {
    name: String,
    decision: String,
}

/// The owner accepts (activates) or rejects (archives) a draft skill.
pub(crate) async fn review_skill(Json(body): Json<SkillReview>) -> ApiResult<Json<Value>> {
    let accept = accept_or_reject(&body.decision)?;
    let msg = workshop::decide_draft(&body.name, accept, "the owner").await.map_err(bad_request)?;
    Ok(Json(json!({ "result": msg })))
}

/// The owner approves (network allowed) or rejects a tool the agent made.
pub(crate) async fn review_tool(State(app): State<AppState>, Path(name): Path<String>, Json(body): Json<Review>) -> ApiResult<Json<Value>> {
    let accept = accept_or_reject(&body.decision)?;
    let msg = workshop::decide_tool(&app.db, &name, accept).await.map_err(bad_request)?;
    Ok(Json(json!({ "result": msg })))
}

/// The routing policy in force, the evidence per kind of work and model, and what it suggests.
pub(crate) async fn get_policy(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let (version, data) = delegate::policy(&app.db).await;
    let stats = delegate::stats(&app.db).await?;
    let suggestions: Vec<Value> = delegate::suggest(&data, &stats, crate::env_num("ZEN_POLICY_MIN_JUDGED", 20.0) as u64)
        .into_iter()
        .map(|(k, m, why)| json!({ "kind": k, "model": m, "why": why }))
        .collect();
    Ok(Json(json!({ "version": version, "policy": data, "stats": stats, "suggestions": suggestions })))
}

#[derive(Deserialize)]
pub(crate) struct PolicyBody {
    policy: Value,
    reason: Option<String>,
}

/// The owner sets the routing policy (a new version).
pub(crate) async fn put_policy(State(app): State<AppState>, Json(body): Json<PolicyBody>) -> ApiResult<Json<Value>> {
    if !body.policy["routes"].is_object() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "the policy needs a `routes` object".into()));
    }
    let v = delegate::set_policy(&app.db, &body.policy, body.reason.as_deref().unwrap_or("set by the owner"), "owner").await?;
    Ok(Json(json!({ "version": v })))
}

/// Go back to the policy before the latest change (as a new version, so nothing is lost).
pub(crate) async fn undo_policy(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let rows: Vec<(i32, Value)> = sqlx::query_as("SELECT version, data FROM policies ORDER BY version DESC LIMIT 2").fetch_all(&app.db).await?;
    let previous = match rows.as_slice() {
        [latest, prev, ..] => Some((latest.0, prev.1.clone())),
        [latest] => Some((latest.0, json!({ "routes": {} }))),
        [] => None,
    };
    let Some((latest, data)) = previous else { return Err(ApiError(StatusCode::CONFLICT, "there is no policy to undo".into())) };
    let v = delegate::set_policy(&app.db, &data, &format!("undo v{latest}"), "owner").await?;
    Ok(Json(json!({ "version": v })))
}

// ---------- scheduled jobs (jobs.rs) ----------

/// Every job with its schedule, next run and last result.
pub(crate) async fn list_jobs(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let jobs = jobs::list(&app.db).await.map_err(bad_request)?;
    Ok(Json(json!({ "jobs": jobs.iter().map(jobs::job_json).collect::<Vec<_>>(), "scheduler": jobs::enabled() })))
}

/// The owner creates an agent job: {name, prompt, schedule, tz?, context?, skills?, model?, workspace?}.
pub(crate) async fn create_job(State(app): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let (job, _) = jobs::create(&app, jobs::Spec::from_json(&body), jobs::By::Owner).await.map_err(bad_request)?;
    Ok(Json(jobs::job_json(&job)))
}

/// The owner changes a job: `enabled` pauses or resumes it; other fields change an agent job.
pub(crate) async fn update_job(State(app): State<AppState>, Path(name): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let spec = jobs::Spec::from_json(&body);
    let changes = spec.prompt.is_some() || spec.schedule.is_some() || spec.tz.is_some() || spec.context.is_some() || spec.skills.is_some() || spec.model.is_some() || spec.workspace.is_some();
    let mut job = if changes { Some(jobs::update(&app, &name, spec, jobs::By::Owner).await.map_err(bad_request)?.0) } else { None };
    if let Some(on) = body["enabled"].as_bool() {
        job = Some(jobs::set_enabled(&app, &name, on, jobs::By::Owner).await.map_err(bad_request)?.0);
    }
    let job = match job {
        Some(j) => j,
        None => jobs::get(&app.db, &name).await.map_err(bad_request)?,
    };
    Ok(Json(jobs::job_json(&job)))
}

pub(crate) async fn delete_job(State(app): State<AppState>, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    jobs::remove(&app.db, &name).await.map_err(bad_request)?;
    Ok(Json(json!({ "removed": name })))
}

/// Run a job now, in the background.
pub(crate) async fn run_job(State(app): State<AppState>, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let run = jobs::run_now(&app, &name, jobs::By::Owner).await.map_err(bad_request)?;
    Ok(Json(json!({ "run": run, "job": name })))
}

#[derive(Deserialize)]
pub(crate) struct RunsQuery {
    job: Option<String>,
    limit: Option<i64>,
}

/// Recent runs, newest first (`?job=` for one job).
pub(crate) async fn list_job_runs(State(app): State<AppState>, Query(q): Query<RunsQuery>) -> ApiResult<Json<Value>> {
    let runs = jobs::runs(&app.db, q.job.as_deref(), q.limit.unwrap_or(20)).await.map_err(bad_request)?;
    Ok(Json(Value::Array(runs)))
}
