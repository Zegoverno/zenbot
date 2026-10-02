//! zend — the zenbot kernel. Owns all state and all side effects.

mod context;
mod mind;
mod score;
mod tools;
mod update;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use uuid::Uuid;

use mind::{Incoming, Mind};

/// A worker process speaking the worker protocol (docs/worker-protocol.md).
/// The process is restarted by `supervise` when it exits, so `mind` is swapped in place.
struct Worker {
    name: String,
    cmd: String,
    dir: String,
    mind: std::sync::RwLock<Arc<Mind>>,
}

impl Worker {
    fn mind(&self) -> Arc<Mind> {
        self.mind.read().unwrap().clone()
    }
}

/// A turn in progress. A session has at most one.
struct Turn {
    worker: usize,
    /// Set to true to stop the kernel's tool calls for this turn (abort, crash, end).
    cancel: watch::Sender<bool>,
    last_activity: Instant,
    tools_running: usize,
    /// When `turn.abort` was sent (by the user or the watchdog); the turn is ended by force if it lingers.
    abort_sent: Option<Instant>,
    model: String,
    /// Instruction files this session has already been given (see context.rs).
    context: HashSet<PathBuf>,
    /// The turn's row in `turns`; its model and tool calls are recorded under it.
    turn_id: Uuid,
    started: Instant,
    /// Model id the provider reported for the turn's latest model call.
    model_resolved: Option<String>,
    /// The worker's `turn.usage` report, if any (engine, version, turn totals).
    reported: Value,
}

struct App {
    db: PgPool,
    workers: Vec<Worker>,
    /// model id -> index into `workers`
    routes: Mutex<HashMap<String, usize>>,
    /// model id -> its entry from `models.list` (name, efforts, default_effort, …)
    catalog: Mutex<HashMap<String, Value>>,
    /// classifier id (System One model, for live scoring) -> index into `workers`
    classifiers: Mutex<HashMap<String, usize>>,
    turns: Mutex<HashMap<Uuid, Turn>>,
    token: String,
    workspace: PathBuf,
    repo: String,
    default_model: String,
    /// What built this kernel, recorded with every turn: ZEN_HARNESS, else the installed commit.
    harness: String,
    hubs: Mutex<HashMap<Uuid, broadcast::Sender<String>>>,
    updater: Arc<update::Updater>,
}

type AppState = Arc<App>;

impl App {
    async fn hub(&self, id: Uuid) -> broadcast::Sender<String> {
        self.hubs.lock().await.entry(id).or_insert_with(|| broadcast::channel(1024).0).clone()
    }

    async fn emit(&self, id: Uuid, event: Value) {
        let _ = self.hub(id).await.send(event.to_string());
    }

    async fn is_busy(&self, id: Uuid) -> bool {
        self.turns.lock().await.contains_key(&id)
    }
}

// ---------- errors ----------

struct ApiError(StatusCode, String);

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

type ApiResult<T> = std::result::Result<T, ApiError>;

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "session not found".into())
}

// ---------- main ----------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "zend=info".into())).init();

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://zen:zen@127.0.0.1:5432/zen".into());
    let token = std::env::var("ZEN_TOKEN").map_err(|_| anyhow::anyhow!("ZEN_TOKEN must be set"))?;
    let port: u16 = std::env::var("ZEN_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8100);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let workspace = PathBuf::from(std::env::var("ZEN_WORKSPACE").unwrap_or_else(|_| home.clone()));
    let repo = std::env::var("ZEN_REPO").unwrap_or_else(|_| format!("{home}/zenbot"));
    let repo_dir = repo.clone();
    let default_model = std::env::var("ZEN_DEFAULT_MODEL").unwrap_or_else(|_| "claude/claude-opus-5-5".into());

    tokio::fs::create_dir_all(&workspace).await?;
    let db = PgPoolOptions::new().max_connections(10).connect(&database_url).await?;
    // A rolled-back build must still start on a database a newer build already migrated.
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator.run(&db).await?;

    let updater = Arc::new(update::Updater::new(PathBuf::from(&repo_dir)));
    let harness = std::env::var("ZEN_HARNESS").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| updater.running());
    let harness = if harness.is_empty() { "unknown".to_string() } else { harness };
    let (merged_tx, incoming) = mpsc::unbounded_channel();
    let mut workers = Vec::new();
    let mut exits = Vec::new();
    for (name, cmd, dir) in worker_configs() {
        match Mind::spawn(&cmd, &dir).await {
            Ok((mind, rx, exited)) => {
                forward_worker(workers.len(), rx, merged_tx.clone());
                tracing::info!("worker `{name}` started: {cmd}");
                exits.push(exited);
                workers.push(Worker { name, cmd, dir, mind: std::sync::RwLock::new(mind) });
            }
            Err(e) => tracing::error!("worker `{name}` failed to start: {e:#}"),
        }
    }
    if workers.is_empty() {
        anyhow::bail!("no workers could be started; check ZEN_WORKERS");
    }
    let app: AppState = Arc::new(App {
        db,
        workers,
        routes: Mutex::new(HashMap::new()),
        catalog: Mutex::new(HashMap::new()),
        classifiers: Mutex::new(HashMap::new()),
        turns: Mutex::new(HashMap::new()),
        token,
        workspace,
        repo,
        default_model,
        harness,
        hubs: Mutex::new(HashMap::new()),
        updater,
    });
    tokio::spawn(app.updater.clone().check_periodically());
    for (idx, exited) in exits.into_iter().enumerate() {
        tokio::spawn(supervise(app.clone(), idx, exited, merged_tx.clone()));
    }
    tokio::spawn(dispatch(app.clone(), incoming));
    tokio::spawn(watchdog(app.clone()));
    tokio::spawn(score::idle_loop(app.clone()));

    let api = Router::new()
        .route("/models", get(list_models))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session).patch(update_session))
        .route("/sessions/{id}/ws", get(session_ws))
        .route("/sessions/{id}/decision", axum::routing::post(decide))
        .route("/version", get(version))
        .route("/upgrade", get(upgrade_status).post(upgrade_start))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth));

    let router = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .nest("/api", api)
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("zend listening on :{port}");
    axum::serve(listener, router).await?;
    Ok(())
}

/// Workers to run, from ZEN_WORKERS (default: `engine`, plus `pi` when it's installed).
/// - engine: zen-engine (Claude Code + Codex CLIs on the owner's subscriptions)
/// - pi:     zen-mind (Pi agent loop; direct ChatGPT sign-in and API providers)
fn worker_configs() -> Vec<(String, String, String)> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
    let mind_dir = std::env::var("ZEN_MIND_DIR").unwrap_or_else(|_| "packages/mind".into());
    let pi_installed = std::path::Path::new(&mind_dir).join("node_modules").exists();
    let default = if pi_installed { "engine,pi" } else { "engine" };
    std::env::var("ZEN_WORKERS")
        .unwrap_or_else(|_| default.into())
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|name| match name {
            "engine" => {
                let cmd = std::env::var("ZEN_ENGINE_CMD").unwrap_or_else(|_| exe_dir.join("zen-engine").display().to_string());
                (name.to_string(), cmd, ".".to_string())
            }
            "pi" => (name.to_string(), std::env::var("ZEN_MIND_CMD").unwrap_or_else(|_| "node src/main.ts".into()), mind_dir.clone()),
            other => {
                let var = format!("ZEN_WORKER_{}_CMD", other.to_uppercase());
                (other.to_string(), std::env::var(&var).unwrap_or_else(|_| other.to_string()), ".".to_string())
            }
        })
        .collect()
}

/// Feed a worker's messages into the kernel's single ordered queue, tagged with its index.
fn forward_worker(idx: usize, mut rx: mpsc::UnboundedReceiver<Incoming>, tx: mpsc::UnboundedSender<(usize, Incoming)>) {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if tx.send((idx, msg)).is_err() {
                break;
            }
        }
    });
}

/// Restart a worker whenever it exits, ending the turns it was running so their sessions
/// don't stay stuck. Backs off when it keeps crashing.
async fn supervise(app: AppState, idx: usize, mut exited: tokio::sync::oneshot::Receiver<()>, tx: mpsc::UnboundedSender<(usize, Incoming)>) {
    let mut backoff = Duration::from_secs(1);
    let mut started = Instant::now();
    loop {
        let _ = (&mut exited).await;
        let w = &app.workers[idx];
        tracing::error!("worker `{}` exited; restarting", w.name);
        let orphans: Vec<Uuid> = app.turns.lock().await.iter().filter(|(_, t)| t.worker == idx).map(|(id, _)| *id).collect();
        for id in orphans {
            finish_turn(&app, id, json!(format!("the `{}` worker crashed during this turn; send your message again", w.name))).await;
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        loop {
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
            match Mind::spawn(&w.cmd, &w.dir).await {
                Ok((mind, rx, ex)) => {
                    forward_worker(idx, rx, tx.clone());
                    *w.mind.write().unwrap() = mind;
                    exited = ex;
                    started = Instant::now();
                    tracing::info!("worker `{}` restarted", w.name);
                    break;
                }
                Err(e) => tracing::error!("worker `{}` failed to restart: {e:#}", w.name),
            }
        }
    }
}

/// Stop turns that have gone quiet: no message from the worker and no tool running for
/// ZEN_TURN_IDLE_SECS (default 600). First ask the worker to abort; if the turn is still
/// there 30s after any abort, end it in the kernel.
async fn watchdog(app: AppState) {
    let idle_limit = Duration::from_secs(std::env::var("ZEN_TURN_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(600));
    let grace = Duration::from_secs(30);
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
async fn finish_turn(app: &App, id: Uuid, error: Value) -> bool {
    let Some(turn) = app.turns.lock().await.remove(&id) else { return false };
    let _ = turn.cancel.send(true);
    let summary = match record_turn(app, &turn, &error).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("recording turn {}: {e:#}", turn.turn_id);
            Value::Null
        }
    };
    let cost: f64 = sqlx::query_scalar(&format!("SELECT {}", session_cost("$1"))).bind(id).fetch_one(&app.db).await.unwrap_or(0.0);
    app.emit(id, json!({ "type": "end", "error": error, "cost": cost, "turn": summary })).await;
    true
}

/// A session's cost: its turns, plus calls recorded before turns were (they have no turn_id).
fn session_cost(session: &str) -> String {
    format!(
        "(COALESCE((SELECT SUM(cost_usd) FROM turns t WHERE t.session_id = {session}), 0)
          + COALESCE((SELECT SUM(cost_usd) FROM model_calls m WHERE m.session_id = {session} AND m.turn_id IS NULL), 0))"
    )
}

/// Close a turn's row: outcome, duration, counts and totals. The worker's own turn report wins
/// where it has a figure (Claude Code's covers side calls the stream never shows); otherwise the
/// totals are summed from the turn's model calls. Returns the row as JSON.
async fn record_turn(app: &App, turn: &Turn, error: &Value) -> Result<Value> {
    let outcome = match error.as_str() {
        None => "ok",
        Some("interrupted") => "interrupted",
        Some(_) => "error",
    };
    let r = &turn.reported;
    let int = |k: &str| r.get(k).and_then(Value::as_i64);
    let row = sqlx::query(
        "WITH calls AS (SELECT COUNT(*)::int AS n, SUM(input_tokens) AS i, SUM(output_tokens) AS o, SUM(cache_read) AS cr,
                               SUM(cache_write) AS cw, SUM(cost_usd) AS c FROM model_calls WHERE turn_id = $1),
              tools AS (SELECT COUNT(*)::int AS n, (COUNT(*) FILTER (WHERE is_error))::int AS e FROM tool_calls WHERE turn_id = $1)
         UPDATE turns SET ended_at = now(), duration_ms = $2, outcome = $3, error = $4,
                engine = $5, engine_version = $6, model_resolved = $7,
                model_calls = (SELECT n FROM calls), tool_calls = (SELECT n FROM tools), tool_errors = (SELECT e FROM tools),
                input_tokens = COALESCE($8, (SELECT i FROM calls), 0), output_tokens = COALESCE($9, (SELECT o FROM calls), 0),
                cache_read = COALESCE($10, (SELECT cr FROM calls), 0), cache_write = COALESCE($11, (SELECT cw FROM calls), 0),
                cost_usd = COALESCE($12, (SELECT c FROM calls), 0), usage = $13
         WHERE id = $1
         RETURNING id, harness, worker, engine, engine_version, model, model_resolved, effort, duration_ms, outcome,
                   model_calls, tool_calls, tool_errors, input_tokens, output_tokens, cache_read, cache_write, cost_usd",
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
    .bind(if r.is_null() { None } else { Some(r) })
    .fetch_one(&app.db)
    .await?;
    Ok(turn_json(&row))
}

fn turn_json(r: &sqlx::postgres::PgRow) -> Value {
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
    })
}

// ---------- auth ----------

async fn auth(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let header_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v) == app.token)
        .unwrap_or(false);
    let query_ok = req
        .uri()
        .query()
        .map(|q| q.split('&').any(|kv| kv == format!("token={}", app.token)))
        .unwrap_or(false);
    if header_ok || query_ok {
        next.run(req).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "unauthorized".into()).into_response()
    }
}

// ---------- handlers ----------

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn health(State(app): State<AppState>) -> Json<Value> {
    let db = sqlx::query("SELECT 1").execute(&app.db).await.is_ok();
    let mut workers = serde_json::Map::new();
    for w in &app.workers {
        workers.insert(w.name.clone(), json!(w.mind().request("ping", json!({})).await.is_ok()));
    }
    let mind = workers.values().all(|v| v == true);
    let busy = app.turns.lock().await.len();
    Json(json!({ "ok": db && mind, "db": db, "mind": mind, "workers": workers, "busy": busy,
                 "version": env!("CARGO_PKG_VERSION"), "commit": app.updater.running() }))
}

#[derive(Deserialize)]
struct VersionQuery {
    refresh: Option<bool>,
}

/// Running version and whether origin/main has newer commits (`?refresh=true` checks now).
async fn version(State(app): State<AppState>, Query(q): Query<VersionQuery>) -> Json<Value> {
    Json(if q.refresh.unwrap_or(false) { app.updater.check().await } else { app.updater.info().await })
}

/// Pull the latest main and apply it (scripts/self-update.sh); progress via GET /api/upgrade.
async fn upgrade_start(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    match app.updater.start().await {
        Ok(started_at) => Ok(Json(json!({ "started_at": started_at }))),
        Err(e) => Err(ApiError(StatusCode::CONFLICT, e)),
    }
}

async fn upgrade_status(State(app): State<AppState>) -> Json<Value> {
    Json(app.updater.status().await)
}

/// Models offered, in order of preference. Engines' models come first; Pi's direct models after.
const DEFAULT_MODELS: &str = "claude/claude-opus-5-5,claude/claude-sonnet-5-5,claude/claude-haiku-4-5-20251001,codex/gpt-6-sol,codex/gpt-6-astra,codex/gpt-6-luna,codex/gpt-5.5,\
openai/gpt-6.1-sol,openai/gpt-6-sol,openai/gpt-6-luna,openai/gpt-6-astra,openai/gpt-5.5";

/// Ask every worker for its models, refresh routing, and return the curated list.
async fn collect_models(app: &App) -> Value {
    let mut all: Vec<(usize, Value)> = Vec::new();
    let mut authenticated = serde_json::Map::new();
    let mut classifiers: Vec<String> = Vec::new();
    for (i, w) in app.workers.iter().enumerate() {
        let Ok(res) = w.mind().request("models.list", json!({})).await else { continue };
        if let Some(a) = res["authenticated"].as_object() {
            authenticated.extend(a.clone());
        }
        let mut routes = app.classifiers.lock().await;
        for id in res["classifiers"].as_array().into_iter().flatten().filter_map(|c| c["id"].as_str()) {
            routes.entry(id.to_string()).or_insert(i);
            classifiers.push(id.to_string());
        }
        drop(routes);
        all.extend(res["models"].as_array().into_iter().flatten().map(|m| (i, m.clone())));
    }
    let mut routes = app.routes.lock().await;
    let mut catalog = app.catalog.lock().await;
    for (i, m) in &all {
        if let Some(id) = m["id"].as_str() {
            routes.entry(id.to_string()).or_insert(*i);
            catalog.entry(id.to_string()).or_insert_with(|| m.clone());
        }
    }
    drop((routes, catalog));
    let wanted = std::env::var("ZEN_MODELS").unwrap_or_else(|_| DEFAULT_MODELS.into());
    let mut curated: Vec<Value> = wanted
        .split(',')
        .filter_map(|id| all.iter().find(|(_, m)| m["id"] == id.trim()).map(|(_, m)| m.clone()))
        .collect();
    curated.extend(all.iter().filter(|(_, m)| m["id"].as_str().is_some_and(|id| id.starts_with("faux/"))).map(|(_, m)| m.clone()));
    let default = if curated.iter().any(|m| m["id"] == app.default_model.as_str()) {
        app.default_model.clone()
    } else {
        curated.first().and_then(|m| m["id"].as_str()).unwrap_or(&app.default_model).to_string()
    };
    json!({ "models": curated, "authenticated": authenticated, "default": default,
            "scorer": score::scorer(), "classifiers": classifiers,
            "workers": app.workers.iter().map(|w| w.name.clone()).collect::<Vec<_>>() })
}

async fn list_models(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(collect_models(&app).await))
}

/// The worker that serves a model (refreshing routes once if it's unknown).
async fn worker_for(app: &App, model: &str) -> Option<usize> {
    if let Some(i) = app.routes.lock().await.get(model) {
        return Some(*i);
    }
    collect_models(app).await;
    app.routes.lock().await.get(model).copied()
}

/// The worker that serves a classifier (refreshing routes once if it's unknown).
async fn worker_for_classifier(app: &App, model: &str) -> Option<usize> {
    if let Some(i) = app.classifiers.lock().await.get(model) {
        return Some(*i);
    }
    collect_models(app).await;
    app.classifiers.lock().await.get(model).copied()
}

/// A model's entry from `models.list` (refreshing once if it's unknown).
async fn model_info(app: &App, model: &str) -> Option<Value> {
    if let Some(m) = app.catalog.lock().await.get(model) {
        return Some(m.clone());
    }
    collect_models(app).await;
    app.catalog.lock().await.get(model).cloned()
}

/// The thinking levels a model accepts, as listed by its worker (empty: it has none to choose).
fn efforts(info: &Value) -> Vec<&str> {
    info["efforts"].as_array().into_iter().flatten().filter_map(Value::as_str).collect()
}

/// Check a session's effort against its model. `None` (the model's default) is always valid.
async fn check_effort(app: &App, model: &str, effort: Option<&str>) -> ApiResult<()> {
    let Some(effort) = effort else { return Ok(()) };
    let info = model_info(app, model).await.unwrap_or(Value::Null);
    let allowed = efforts(&info);
    if allowed.contains(&effort) {
        return Ok(());
    }
    let msg = if allowed.is_empty() {
        format!("model `{model}` has no thinking levels to choose from")
    } else {
        format!("model `{model}` takes effort {}; got `{effort}`", allowed.join(", "))
    };
    Err(ApiError(StatusCode::BAD_REQUEST, msg))
}

#[derive(Deserialize)]
struct ListQuery {
    archived: Option<bool>,
}

async fn list_sessions(State(app): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(&format!(
        "SELECT s.id, s.title, s.model, s.effort, s.archived, s.created_at, s.updated_at,
                {} AS cost
         FROM sessions s WHERE s.archived = $1 ORDER BY s.updated_at DESC",
        session_cost("s.id")
    ))
    .bind(q.archived.unwrap_or(false))
    .fetch_all(&app.db)
    .await?;
    Ok(Json(Value::Array(rows.iter().map(session_json).collect())))
}

fn session_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "title": r.get::<String, _>("title"),
        "model": r.get::<String, _>("model"),
        "effort": r.get::<Option<String>, _>("effort"),
        "archived": r.get::<bool, _>("archived"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        "cost": r.try_get::<f64, _>("cost").unwrap_or(0.0),
    })
}

#[derive(Deserialize)]
struct CreateSession {
    title: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

async fn create_session(State(app): State<AppState>, Json(body): Json<CreateSession>) -> ApiResult<Json<Value>> {
    let id = Uuid::new_v4();
    let model = body.model.unwrap_or_else(|| app.default_model.clone());
    check_effort(&app, &model, body.effort.as_deref()).await?;
    let row = sqlx::query(
        "INSERT INTO sessions (id, title, model, effort) VALUES ($1, $2, $3, $4)
         RETURNING id, title, model, effort, archived, created_at, updated_at, 0::float8 AS cost",
    )
    .bind(id)
    .bind(body.title.unwrap_or_default())
    .bind(model)
    .bind(body.effort)
    .fetch_one(&app.db)
    .await?;
    Ok(Json(session_json(&row)))
}

#[derive(Deserialize)]
struct UpdateSession {
    title: Option<String>,
    model: Option<String>,
    /// A thinking level, or "default" for the model's default.
    effort: Option<String>,
    archived: Option<bool>,
}

async fn update_session(State(app): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<UpdateSession>) -> ApiResult<Json<Value>> {
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
        "UPDATE sessions SET title = COALESCE($2, title), model = COALESCE($3, model), effort = $5,
                archived = COALESCE($4, archived), updated_at = now()
         WHERE id = $1
         RETURNING id, title, model, effort, archived, created_at, updated_at, {} AS cost",
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

async fn get_session(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = sqlx::query(&format!(
        "SELECT id, title, model, effort, archived, created_at, updated_at, {} AS cost
         FROM sessions WHERE id = $1",
        session_cost("$1")
    ))
    .bind(id)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(not_found)?;
    let mut session = session_json(&row);
    session["messages"] = Value::Array(load_messages(&app.db, id).await?);
    session["busy"] = json!(app.is_busy(id).await);
    Ok(Json(session))
}

/// What the owner decides about the work so far (see migrations/0005).
const DECISIONS: [&str; 4] = ["accept", "more", "reshape", "drop"];

#[derive(Deserialize)]
struct Decide {
    decision: String,
    note: Option<String>,
}

/// Record the owner's decision on the session's work up to its latest turn.
async fn decide(State(app): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<Decide>) -> ApiResult<Json<Value>> {
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
    // Score the work the decision covers, so each decision has a score to compare it with.
    let scoring = app.clone();
    tokio::spawn(async move {
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

async fn load_messages(db: &PgPool, id: Uuid) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query("SELECT payload FROM tape_events WHERE session_id = $1 AND kind = 'message' ORDER BY id")
        .bind(id)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|r| r.get::<Value, _>("payload")).collect())
}

/// Keep long sessions within the context window: older tool outputs are trimmed
/// (the full output stays in the tape). Recent messages are sent untouched.
fn prune_history(mut history: Vec<Value>) -> Vec<Value> {
    const KEEP_RECENT: usize = 12;
    const MAX_OLD_OUTPUT: usize = 400;
    let cutoff = history.len().saturating_sub(KEEP_RECENT);
    for m in history.iter_mut().take(cutoff) {
        if m["role"] != "toolResult" {
            continue;
        }
        if let Some(parts) = m["content"].as_array_mut() {
            for part in parts.iter_mut() {
                if let Some(text) = part["text"].as_str() {
                    if text.len() > MAX_OLD_OUTPUT {
                        let mut end = 300;
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        part["text"] = json!(format!("{}\n[... {} more bytes trimmed from history ...]", &text[..end], text.len() - end));
                    }
                }
            }
        }
    }
    history
}

/// Instruction files recorded for a session (tape kind `context`), oldest first.
async fn load_context(db: &PgPool, id: Uuid) -> Result<Vec<PathBuf>, sqlx::Error> {
    let rows = sqlx::query("SELECT payload FROM tape_events WHERE session_id = $1 AND kind = 'context' ORDER BY id")
        .bind(id)
        .fetch_all(db)
        .await?;
    let mut out: Vec<PathBuf> = Vec::new();
    for r in rows {
        if let Some(p) = r.get::<Value, _>("payload")["path"].as_str().map(PathBuf::from) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

async fn append_tape(db: &PgPool, id: Uuid, kind: &str, payload: &Value) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO tape_events (session_id, kind, payload) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(kind)
        .bind(payload)
        .execute(db)
        .await?;
    sqlx::query("UPDATE sessions SET updated_at = now() WHERE id = $1").bind(id).execute(db).await?;
    Ok(())
}

// ---------- websocket ----------

async fn session_ws(State(app): State<AppState>, Path(id): Path<Uuid>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(app, id, socket))
}

async fn handle_socket(app: AppState, id: Uuid, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let mut rx = app.hub(id).await.subscribe();
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
                let text = cmd.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                if let Err(e) = start_turn(&app, id, text).await {
                    app.emit(id, json!({ "type": "error", "error": e.to_string() })).await;
                }
            }
            Some("abort") => {
                // Stop the kernel's tools right away, then ask the worker to stop the model.
                let worker = app.turns.lock().await.get_mut(&id).map(|t| {
                    let _ = t.cancel.send(true);
                    t.abort_sent.get_or_insert_with(Instant::now);
                    t.worker
                });
                if let Some(w) = worker.and_then(|i| app.workers.get(i)) {
                    let _ = w.mind().request("turn.abort", json!({ "session_id": id })).await;
                }
            }
            _ => {}
        }
    }
    forward.abort();
}

/// The system prompt. `extra` are instruction files this session picked up on demand.
fn system_prompt(workspace: &std::path::Path, repo: &str, extra: &[PathBuf]) -> String {
    let mut s = format!(
        "You are zenbot, the owner's personal agent running on their Linux VM.\n\
         You can run shell commands and read, write, edit and move files using your tools.\n\
         Be concise and direct. Show file paths clearly. Prefer doing the work over describing it.\n\
         The user sees every tool call and its full output in the interface, so never repeat raw tool output; \
         summarize what matters and quote only the relevant lines.\n\
         Ask before destructive or outward-facing actions (deleting data, pushing, publishing, sending messages, spending money).\n\
         Your own source code (zenbot) is at {repo}. Before changing yourself, read {repo}/AGENTS.md and follow it; \
         never restart your own service directly, use the upgrade script it describes.\n\
         \n\
         <tool_guidelines>\n\
         - Use read to look at files (not cat or sed), and read a file before editing it.\n\
         - Use edit for changes to existing files and write for new files or complete rewrites. Edits to the same file are applied one at a time, so several in one step are safe.\n\
         - Use bash for searching (rg, grep, find), git, builds, tests and running programs.\n\
         - Start servers and other long-running processes in the background with output redirected to a file.\n\
         - When output is cut, the result says where the full output was saved or which offset to read next.\n\
         </tool_guidelines>\n"
    );
    let mut files = context::always(workspace);
    files.extend(extra.iter().filter_map(|p| context::read_capped(p).map(|t| (p.clone(), t))));
    if !files.is_empty() {
        s.push_str("\n<project_context>\nInstructions the owner keeps for agents. Follow them.\n");
        for (path, text) in files {
            s.push_str(&format!("<file path=\"{}\">\n{}\n</file>\n", path.display(), text.trim_end()));
        }
        s.push_str("</project_context>\n");
    }
    s.push_str(&format!(
        "\nWorking directory for tools: {} (paths are relative to it unless absolute; ~ is the home directory).\nToday is {}.",
        workspace.display(),
        chrono::Local::now().format("%Y-%m-%d (%A)")
    ));
    s
}

async fn start_turn(app: &AppState, id: Uuid, text: String) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let row = sqlx::query("SELECT model, effort, title FROM sessions WHERE id = $1").bind(id).fetch_optional(&app.db).await?;
    let Some(row) = row else { anyhow::bail!("session not found") };
    let model: String = row.get("model");
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
    {
        let mut turns = app.turns.lock().await;
        if turns.contains_key(&id) {
            anyhow::bail!("this session is already working; wait or abort");
        }
        turns.insert(
            id,
            Turn {
                worker: widx,
                cancel: watch::channel(false).0,
                last_activity: Instant::now(),
                tools_running: 0,
                abort_sent: None,
                model: model.clone(),
                context: HashSet::new(),
                turn_id,
                started: Instant::now(),
                model_resolved: None,
                reported: Value::Null,
            },
        );
    }
    let res = async {
        sqlx::query("INSERT INTO turns (id, session_id, harness, worker, model, effort) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(turn_id)
            .bind(id)
            .bind(&app.harness)
            .bind(&app.workers[widx].name)
            .bind(&model)
            .bind(&effort)
            .execute(&app.db)
            .await?;
        let history = prune_history(load_messages(&app.db, id).await?);
        let extra = load_context(&app.db, id).await?;
        if let Some(t) = app.turns.lock().await.get_mut(&id) {
            t.context = extra.iter().cloned().collect();
        }
        let user = json!({ "role": "user", "content": text, "timestamp": chrono::Utc::now().timestamp_millis() });
        append_tape(&app.db, id, "message", &user).await?;
        if title.is_empty() {
            let t: String = text.chars().take(60).collect();
            sqlx::query("UPDATE sessions SET title = $2 WHERE id = $1").bind(id).bind(t.trim()).execute(&app.db).await?;
        }
        app.emit(id, json!({ "type": "message", "message": user })).await;
        app.workers[widx]
            .mind()
            .request(
                "turn.start",
                json!({
                    "session_id": id,
                    "model": model,
                    "effort": effort,
                    "system_prompt": system_prompt(&app.workspace, &app.repo, &extra),
                    "history": history,
                    "prompt": text,
                    "tools": tools::specs(),
                }),
            )
            .await
    }
    .await;
    if let Err(e) = res {
        // The turn never started; close its row (if it was written) as an error.
        if let Some(turn) = app.turns.lock().await.remove(&id) {
            let _ = record_turn(app, &turn, &json!(e.to_string())).await;
        }
        return Err(e);
    }
    app.emit(id, json!({ "type": "busy", "busy": true, "turn_id": turn_id, "harness": app.harness, "model": model, "effort": effort })).await;
    Ok(())
}

// ---------- events from the worker ----------

async fn dispatch(app: AppState, mut incoming: tokio::sync::mpsc::UnboundedReceiver<(usize, Incoming)>) {
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

fn session_id(params: &Value) -> Result<Uuid> {
    Ok(params.get("session_id").and_then(Value::as_str).unwrap_or_default().parse()?)
}

/// Note activity on a session's turn. Returns false when the message doesn't belong to the turn
/// running on that worker (e.g. a late message from a turn that was already ended); such messages
/// are dropped so they can't leak into the session.
async fn touch(app: &App, id: Uuid, worker: usize) -> bool {
    match app.turns.lock().await.get_mut(&id) {
        Some(t) if t.worker == worker => {
            t.last_activity = Instant::now();
            true
        }
        _ => false,
    }
}

async fn handle_incoming(app: &AppState, worker: usize, msg: Incoming) -> Result<()> {
    let mind = app.workers[worker].mind();
    let p = &msg.params;
    let id = session_id(p);
    if let Ok(id) = id {
        if !touch(app, id, worker).await {
            tracing::warn!("dropping `{}` for session {id}: no turn running on that worker", msg.method);
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
            let (mut cancel, model, turn_id) = {
                let mut turns = app.turns.lock().await;
                let Some(t) = turns.get_mut(&id) else { return Ok(()) };
                t.tools_running += 1;
                (t.cancel.subscribe(), t.model.clone(), t.turn_id)
            };
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
                    out = tools::execute(&app.workspace, &name, &args, &env) => out,
                    _ = cancel.wait_for(|c| *c) => tools::ToolOutput { content: "interrupted: the turn was stopped before this tool finished".into(), is_error: true },
                }
            };
            let ms = started.elapsed().as_millis() as i64;
            // First time this session touches a project with its own instructions: attach them.
            let new_context: Vec<PathBuf> = {
                let candidates = context::governing(&app.workspace, &context::paths_in_call(&app.workspace, &name, &args));
                let mut turns = app.turns.lock().await;
                match turns.get_mut(&id) {
                    Some(t) => {
                        t.tools_running = t.tools_running.saturating_sub(1);
                        t.last_activity = Instant::now();
                        candidates.into_iter().filter(|p| t.context.insert(p.clone())).collect()
                    }
                    None => Vec::new(),
                }
            };
            for path in new_context {
                if let Some(text) = context::read_capped(&path) {
                    append_tape(&app.db, id, "context", &json!({ "path": path })).await?;
                    out.content.push_str(&context::attachment(&path, &text));
                }
            }
            sqlx::query(
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
            .await?;
            app.emit(id, json!({ "type": "tool_end", "call_id": call_id, "is_error": out.is_error, "ms": ms })).await;
            if let Some(req_id) = msg.id {
                mind.respond(req_id, json!({ "content": out.content, "is_error": out.is_error })).await?;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_includes_context_files_from_ancestors() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("zend-ctx-{nanos}"));
        let ws = root.join("proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "outer rule").unwrap();
        std::fs::write(ws.join("AGENTS.md"), "inner rule").unwrap();
        std::fs::write(ws.join("CLAUDE.md"), "shadowed by AGENTS.md").unwrap();
        let prompt = system_prompt(&ws, "/repo", &[]);
        let outer = prompt.find("outer rule").expect("parent CLAUDE.md loaded");
        let inner = prompt.find("inner rule").expect("workspace AGENTS.md loaded");
        assert!(outer < inner, "files are ordered from the root down");
        assert!(!prompt.contains("shadowed"));
        assert!(prompt.contains(&format!("Working directory for tools: {}", ws.display())));
    }
}
