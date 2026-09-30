//! zend — the zenbot kernel. Owns all state and all side effects.

mod mind;
mod tools;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use mind::{Incoming, Mind};

struct App {
    db: PgPool,
    mind: Arc<Mind>,
    token: String,
    workspace: PathBuf,
    default_model: String,
    hubs: Mutex<HashMap<Uuid, broadcast::Sender<String>>>,
    busy: Mutex<HashSet<Uuid>>,
}

type AppState = Arc<App>;

impl App {
    async fn hub(&self, id: Uuid) -> broadcast::Sender<String> {
        self.hubs.lock().await.entry(id).or_insert_with(|| broadcast::channel(1024).0).clone()
    }

    async fn emit(&self, id: Uuid, event: Value) {
        let _ = self.hub(id).await.send(event.to_string());
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
    let workspace = PathBuf::from(std::env::var("ZEN_WORKSPACE").unwrap_or_else(|_| format!("{home}/zen-workspace")));
    let mind_cmd = std::env::var("ZEN_MIND_CMD").unwrap_or_else(|_| "node src/main.ts".into());
    let mind_dir = std::env::var("ZEN_MIND_DIR").unwrap_or_else(|_| "packages/mind".into());
    let default_model = std::env::var("ZEN_DEFAULT_MODEL").unwrap_or_else(|_| "openai/gpt-5.5".into());

    tokio::fs::create_dir_all(&workspace).await?;
    let db = PgPoolOptions::new().max_connections(10).connect(&database_url).await?;
    sqlx::migrate!("./migrations").run(&db).await?;

    let (mind, incoming) = Mind::spawn(&mind_cmd, &mind_dir).await?;
    let app: AppState = Arc::new(App {
        db,
        mind,
        token,
        workspace,
        default_model,
        hubs: Mutex::new(HashMap::new()),
        busy: Mutex::new(HashSet::new()),
    });
    tokio::spawn(dispatch(app.clone(), incoming));

    let api = Router::new()
        .route("/models", get(list_models))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session).patch(update_session))
        .route("/sessions/{id}/ws", get(session_ws))
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
    let mind = app.mind.request("ping", json!({})).await.is_ok();
    Json(json!({ "ok": db && mind, "db": db, "mind": mind }))
}

async fn list_models(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let mut res = app.mind.request("models.list", json!({})).await?;
    res["default"] = json!(app.default_model);
    Ok(Json(res))
}

#[derive(Deserialize)]
struct ListQuery {
    archived: Option<bool>,
}

async fn list_sessions(State(app): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(
        "SELECT s.id, s.title, s.model, s.archived, s.created_at, s.updated_at,
                COALESCE((SELECT SUM(cost_usd) FROM model_calls m WHERE m.session_id = s.id), 0) AS cost
         FROM sessions s WHERE s.archived = $1 ORDER BY s.updated_at DESC",
    )
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
}

async fn create_session(State(app): State<AppState>, Json(body): Json<CreateSession>) -> ApiResult<Json<Value>> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        "INSERT INTO sessions (id, title, model) VALUES ($1, $2, $3)
         RETURNING id, title, model, archived, created_at, updated_at, 0::float8 AS cost",
    )
    .bind(id)
    .bind(body.title.unwrap_or_default())
    .bind(body.model.unwrap_or_else(|| app.default_model.clone()))
    .fetch_one(&app.db)
    .await?;
    Ok(Json(session_json(&row)))
}

#[derive(Deserialize)]
struct UpdateSession {
    title: Option<String>,
    model: Option<String>,
    archived: Option<bool>,
}

async fn update_session(State(app): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<UpdateSession>) -> ApiResult<Json<Value>> {
    let row = sqlx::query(
        "UPDATE sessions SET title = COALESCE($2, title), model = COALESCE($3, model),
                archived = COALESCE($4, archived), updated_at = now()
         WHERE id = $1
         RETURNING id, title, model, archived, created_at, updated_at,
                   COALESCE((SELECT SUM(cost_usd) FROM model_calls m WHERE m.session_id = $1), 0) AS cost",
    )
    .bind(id)
    .bind(body.title)
    .bind(body.model)
    .bind(body.archived)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(not_found)?;
    Ok(Json(session_json(&row)))
}

async fn get_session(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = sqlx::query(
        "SELECT id, title, model, archived, created_at, updated_at,
                COALESCE((SELECT SUM(cost_usd) FROM model_calls m WHERE m.session_id = $1), 0) AS cost
         FROM sessions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(not_found)?;
    let mut session = session_json(&row);
    session["messages"] = Value::Array(load_messages(&app.db, id).await?);
    session["busy"] = json!(app.busy.lock().await.contains(&id));
    Ok(Json(session))
}

async fn load_messages(db: &PgPool, id: Uuid) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query("SELECT payload FROM tape_events WHERE session_id = $1 AND kind = 'message' ORDER BY id")
        .bind(id)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|r| r.get::<Value, _>("payload")).collect())
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
    let forward = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
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
                let _ = app.mind.request("turn.abort", json!({ "session_id": id })).await;
            }
            _ => {}
        }
    }
    forward.abort();
}

fn system_prompt(workspace: &std::path::Path) -> String {
    format!(
        "You are zenbot, the owner's personal agent running on their Linux VM.\n\
         You can run shell commands and read, write, edit and move files using your tools.\n\
         The working directory for tools is {} (paths are relative to it unless absolute).\n\
         Be concise and direct. Show file paths clearly. Prefer doing the work over describing it.\n\
         Read files before editing them. Ask before destructive or outward-facing actions \
         (deleting data, pushing, publishing, sending messages, spending money).\n\
         Today is {}.",
        workspace.display(),
        chrono::Utc::now().format("%Y-%m-%d")
    )
}

async fn start_turn(app: &AppState, id: Uuid, text: String) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let row = sqlx::query("SELECT model, title FROM sessions WHERE id = $1").bind(id).fetch_optional(&app.db).await?;
    let Some(row) = row else { anyhow::bail!("session not found") };
    let model: String = row.get("model");
    let title: String = row.get("title");

    if !app.busy.lock().await.insert(id) {
        anyhow::bail!("this session is already working; wait or abort");
    }
    let history = load_messages(&app.db, id).await?;
    let user = json!({ "role": "user", "content": text, "timestamp": chrono::Utc::now().timestamp_millis() });
    append_tape(&app.db, id, "message", &user).await?;
    if title.is_empty() {
        let t: String = text.chars().take(60).collect();
        sqlx::query("UPDATE sessions SET title = $2 WHERE id = $1").bind(id).bind(t.trim()).execute(&app.db).await?;
    }
    app.emit(id, json!({ "type": "message", "message": user })).await;

    let res = app
        .mind
        .request(
            "turn.start",
            json!({
                "session_id": id,
                "model": model,
                "system_prompt": system_prompt(&app.workspace),
                "history": history,
                "prompt": text,
                "tools": tools::specs(),
            }),
        )
        .await;
    if let Err(e) = res {
        app.busy.lock().await.remove(&id);
        return Err(e);
    }
    app.emit(id, json!({ "type": "busy", "busy": true })).await;
    Ok(())
}

// ---------- events from the worker ----------

async fn dispatch(app: AppState, mut incoming: tokio::sync::mpsc::UnboundedReceiver<Incoming>) {
    // Notifications are handled in arrival order so streams and the tape stay ordered.
    // Tool calls run concurrently; the worker already emitted the assistant message that requested them.
    while let Some(msg) = incoming.recv().await {
        if msg.method == "tool.call" {
            let app = app.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_incoming(&app, msg).await {
                    tracing::error!("handling tool call: {e:#}");
                }
            });
        } else if let Err(e) = handle_incoming(&app, msg).await {
            tracing::error!("handling mind message: {e:#}");
        }
    }
}

fn session_id(params: &Value) -> Result<Uuid> {
    Ok(params.get("session_id").and_then(Value::as_str).unwrap_or_default().parse()?)
}

async fn handle_incoming(app: &AppState, msg: Incoming) -> Result<()> {
    let p = &msg.params;
    match msg.method.as_str() {
        "tool.call" => {
            let id = session_id(p)?;
            let name = p.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let call_id = p.get("call_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let args = p.get("args").cloned().unwrap_or(json!({}));
            app.emit(id, json!({ "type": "tool_start", "call_id": call_id, "name": name, "args": args })).await;
            let started = Instant::now();
            let out = tools::execute(&app.workspace, &name, &args).await;
            let ms = started.elapsed().as_millis() as i64;
            sqlx::query(
                "INSERT INTO tool_calls (session_id, call_id, name, args, is_error, duration_ms, output_bytes)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(id)
            .bind(&call_id)
            .bind(&name)
            .bind(&args)
            .bind(out.is_error)
            .bind(ms)
            .bind(out.content.len() as i64)
            .execute(&app.db)
            .await?;
            app.emit(id, json!({ "type": "tool_end", "call_id": call_id, "is_error": out.is_error, "ms": ms })).await;
            if let Some(req_id) = msg.id {
                app.mind.respond(req_id, json!({ "content": out.content, "is_error": out.is_error })).await?;
            }
        }
        "turn.delta" | "turn.thinking" => {
            let id = session_id(p)?;
            let kind = if msg.method == "turn.delta" { "delta" } else { "thinking" };
            app.emit(id, json!({ "type": kind, "delta": p.get("delta") })).await;
        }
        "turn.message" => {
            let id = session_id(p)?;
            let message = p.get("message").cloned().unwrap_or(Value::Null);
            append_tape(&app.db, id, "message", &message).await?;
            if message.get("role").and_then(Value::as_str) == Some("assistant") {
                let u = &message["usage"];
                let n = |k: &str| u.get(k).and_then(Value::as_i64).unwrap_or(0);
                sqlx::query(
                    "INSERT INTO model_calls (session_id, provider, model, input_tokens, output_tokens, cache_read, cache_write, cost_usd, stop_reason)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
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
                .execute(&app.db)
                .await?;
            }
            app.emit(id, json!({ "type": "message", "message": message })).await;
        }
        "turn.end" => {
            let id = session_id(p)?;
            app.busy.lock().await.remove(&id);
            let cost: f64 = sqlx::query_scalar("SELECT COALESCE(SUM(cost_usd), 0) FROM model_calls WHERE session_id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await?;
            app.emit(id, json!({ "type": "end", "error": p.get("error"), "cost": cost })).await;
        }
        other => {
            tracing::warn!("unknown method from mind: {other}");
            if let Some(req_id) = msg.id {
                app.mind.respond(req_id, json!({ "content": format!("unknown method {other}"), "is_error": true })).await?;
            }
        }
    }
    Ok(())
}
