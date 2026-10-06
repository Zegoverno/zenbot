//! zend — the zenbot kernel. Owns all state and all side effects.

mod api;
mod compact;
mod compile;
mod context;
mod dispatch;
mod flow;
mod git;
mod measure;
mod mind;
mod score;
mod secrets;
mod tape;
mod tools;
mod turns;
mod update;
mod workers;

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

use api::*;
use dispatch::*;
use turns::*;
use workers::*;

struct App {
    db: PgPool,
    workers: Vec<Worker>,
    /// model or classifier id -> index into `workers`
    routes: Mutex<HashMap<String, usize>>,
    /// model id -> its entry from `models.list` (name, efforts, default_effort, …)
    catalog: Mutex<HashMap<String, Value>>,
    turns: Mutex<HashMap<Uuid, Turn>>,
    token: String,
    workspace: PathBuf,
    repo: String,
    default_model: String,
    /// What built this kernel, recorded with every turn: ZEN_HARNESS, else the installed commit.
    harness: String,
    /// Event channels of sessions with clients connected; removed when the last one leaves.
    hubs: Mutex<HashMap<Uuid, broadcast::Sender<String>>>,
    updater: Arc<update::Updater>,
    /// Sessions whose summary is being prepared in the background.
    compacting: Mutex<HashSet<Uuid>>,
    /// Kernel-started turns being waited for (verifier sessions), by session.
    waiters: Mutex<HashMap<Uuid, tokio::sync::oneshot::Sender<()>>>,
    /// Sessions with workflow steps running outside a turn (approval, verification), counted as
    /// busy so an upgrade waits for them.
    pub(crate) background: Mutex<HashSet<Uuid>>,
}

type AppState = Arc<App>;

/// A number from a setting, or the default when it's unset or not a number.
pub(crate) fn env_num(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

impl App {
    /// Listen to a session's events (a client's WebSocket). Call `unsubscribe` when it closes.
    async fn subscribe(&self, id: Uuid) -> broadcast::Receiver<String> {
        self.hubs.lock().await.entry(id).or_insert_with(|| broadcast::channel(1024).0).subscribe()
    }

    /// Forget a session's hub once its last client has gone (the receiver must be dropped first).
    async fn unsubscribe(&self, id: Uuid) {
        let mut hubs = self.hubs.lock().await;
        if hubs.get(&id).is_some_and(|h| h.receiver_count() == 0) {
            hubs.remove(&id);
        }
    }

    /// Send an event to a session's clients, if any are listening.
    async fn emit(&self, id: Uuid, event: Value) {
        if let Some(hub) = self.hubs.lock().await.get(&id) {
            let _ = hub.send(event.to_string());
        }
    }

    async fn is_busy(&self, id: Uuid) -> bool {
        self.turns.lock().await.contains_key(&id)
    }
}

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
    if tape::repair(&db).await? > 0 {
        tracing::warn!("numbered tape blocks an older build wrote");
    }

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
        turns: Mutex::new(HashMap::new()),
        token,
        workspace,
        repo,
        default_model,
        harness,
        hubs: Mutex::new(HashMap::new()),
        updater,
        compacting: Mutex::new(HashSet::new()),
        waiters: Mutex::new(HashMap::new()),
        background: Mutex::new(HashSet::new()),
    });
    // A verification a restart cut short can't resume: send the work back so the session isn't stuck.
    for id in sqlx::query_scalar::<_, Uuid>("SELECT id FROM sessions WHERE state = 'verifying'").fetch_all(&app.db).await? {
        flow::set_state(&app, id, "working", "kernel", "verification was interrupted by a restart; submit the work again", false).await?;
    }
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
        .route("/sessions/{id}/flow", axum::routing::post(flow_action))
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

/// A session's messages in order, each with its block number (`seq`).
async fn load_messages(db: &PgPool, id: Uuid) -> Result<Vec<Value>, sqlx::Error> {
    Ok(tape::load(db, id, &["message"])
        .await?
        .into_iter()
        .map(|b| {
            let mut m = b.payload;
            m["seq"] = json!(b.seq);
            m
        })
        .collect())
}

/// Instruction files recorded for a session (tape kind `context`), oldest first.
fn context_in(blocks: &[tape::Block]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for b in blocks.iter().filter(|b| b.kind == "context") {
        if let Some(p) = b.payload["path"].as_str().map(PathBuf::from) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

async fn append_tape(db: &PgPool, id: Uuid, kind: &str, payload: &Value) -> Result<(), sqlx::Error> {
    tape::append(db, id, kind, payload).await.map(|_| ())
}
