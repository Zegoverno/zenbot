//! Workers: starting and supervising model workers, and routing models to them
//! (docs/worker-protocol.md). System One runs in the kernel (score.rs).

use super::*;

use std::collections::HashSet;

/// A worker process speaking the worker protocol (docs/worker-protocol.md).
/// The process is restarted by `supervise` when it exits, so `mind` is swapped in place.
pub(crate) struct Worker {
    pub(crate) name: String,
    pub(crate) cmd: String,
    pub(crate) dir: String,
    pub(crate) mind: std::sync::RwLock<Arc<Mind>>,
}

impl Worker {
    pub(crate) fn mind(&self) -> Arc<Mind> {
        self.mind.read().unwrap().clone()
    }
}

/// Workers to run, from ZEN_WORKERS (default: `engine`).
pub(crate) fn worker_configs() -> Vec<(String, String, String)> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
    let configured = std::env::var("ZEN_WORKERS").unwrap_or_else(|_| "engine".into());
    let mut names: Vec<&str> = configured.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
    if names.is_empty() { names.push("engine"); }
    names.into_iter().map(|name| match name {
            "engine" => {
                let cmd = std::env::var("ZEN_ENGINE_CMD").unwrap_or_else(|_| exe_dir.join("zen-engine").display().to_string());
                (name.to_string(), cmd, ".".to_string())
            }
            other => {
                let var = format!("ZEN_WORKER_{}_CMD", other.to_uppercase());
                (other.to_string(), std::env::var(&var).unwrap_or_else(|_| other.to_string()), ".".to_string())
            }
        })
        .collect()
}

/// Feed a worker's messages into the kernel's single ordered queue, tagged with its index.
pub(crate) fn forward_worker(idx: usize, mut rx: mpsc::UnboundedReceiver<Incoming>, tx: mpsc::UnboundedSender<(usize, Incoming)>) {
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
pub(crate) async fn supervise(app: AppState, idx: usize, mut exited: tokio::sync::oneshot::Receiver<()>, tx: mpsc::UnboundedSender<(usize, Incoming)>) {
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

/// Ask the worker that serves `model` for one completion without tools (`complete`, used for
/// summaries). Returns `{ text, usage, model }`.
pub(crate) async fn complete(app: &App, model: &str, system: &str, prompt: &str) -> Result<Value> {
    let Some(w) = worker_for(app, model).await else { anyhow::bail!("no worker serves `{model}`") };
    let res = app.workers[w]
        .mind()
        .request_within("complete", json!({ "model": model, "system": system, "prompt": prompt }), Duration::from_secs(600))
        .await?;
    if let Some(e) = res["error"].as_str() {
        anyhow::bail!("{e}");
    }
    Ok(res)
}

fn visible_models(all: &[(usize, Value)], wanted: Option<&str>) -> Vec<Value> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |model: &Value| {
        if let Some(id) = model["id"].as_str() {
            if seen.insert(id.to_string()) {
                result.push(model.clone());
            }
        }
    };
    if let Some(wanted) = wanted.filter(|s| !s.trim().is_empty()) {
        for id in wanted.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if let Some((_, model)) = all.iter().find(|(_, model)| model["id"] == id) {
                add(model);
            }
        }
    } else {
        for (_, model) in all {
            add(model);
        }
    }
    for (_, model) in all.iter().filter(|(_, model)| model["id"].as_str().is_some_and(|id| id.starts_with("faux/"))) {
        add(model);
    }
    result
}

/// Ask every worker for its models, refresh routing, and return every available model unless
/// `ZEN_MODELS` explicitly curates the visible list. Routing retains the full catalog either way.
pub(crate) async fn collect_models(app: &App) -> Value {
    let mut all: Vec<(usize, Value)> = Vec::new();
    let mut authenticated = serde_json::Map::new();
    for (i, w) in app.workers.iter().enumerate() {
        let Ok(res) = w.mind().request("models.list", json!({})).await else { continue };
        if let Some(a) = res["authenticated"].as_object() {
            authenticated.extend(a.clone());
        }
        all.extend(res["models"].as_array().into_iter().flatten().map(|m| (i, m.clone())));
    }
    // Rebuild after each worker refresh: an engine update may change a model's efforts,
    // context window or serving worker. Keep first-worker preference for duplicate ids.
    let mut routes = HashMap::new();
    let mut catalog = HashMap::new();
    for (i, m) in &all {
        if let Some(id) = m["id"].as_str() {
            routes.entry(id.to_string()).or_insert(*i);
            catalog.entry(id.to_string()).or_insert_with(|| m.clone());
        }
    }
    // OpenRouter is called by the kernel only (workers run without its key), so the kernel reports it.
    authenticated.insert("openrouter".into(), json!(score::openrouter_key()));
    *app.routes.lock().await = routes;
    *app.catalog.lock().await = catalog;
    let curated = visible_models(&all, std::env::var("ZEN_MODELS").ok().as_deref());
    let default = if curated.iter().any(|m| m["id"] == app.default_model.as_str()) {
        app.default_model.clone()
    } else {
        curated.first().and_then(|m| m["id"].as_str()).unwrap_or(&app.default_model).to_string()
    };
    json!({ "models": curated, "authenticated": authenticated, "default": default,
            "scorer": score::scorer() })
}

/// The worker that serves a model (refreshing routes once if it's unknown).
pub(crate) async fn worker_for(app: &App, model: &str) -> Option<usize> {
    if let Some(i) = app.routes.lock().await.get(model) {
        return Some(*i);
    }
    collect_models(app).await;
    app.routes.lock().await.get(model).copied()
}

/// A model's entry from `models.list` (refreshing once if it's unknown).
pub(crate) async fn model_info(app: &App, model: &str) -> Option<Value> {
    if let Some(m) = app.catalog.lock().await.get(model) {
        return Some(m.clone());
    }
    collect_models(app).await;
    app.catalog.lock().await.get(model).cloned()
}

/// The thinking levels a model accepts, as listed by its worker (empty: it has none to choose).
pub(crate) fn efforts(info: &Value) -> Vec<&str> {
    info["efforts"].as_array().into_iter().flatten().filter_map(Value::as_str).collect()
}

/// Check a session's effort against its model. `None` (the model's default) is always valid.
pub(crate) async fn check_effort(app: &App, model: &str, effort: Option<&str>) -> ApiResult<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(models: &[Value]) -> Vec<&str> {
        models.iter().filter_map(|m| m["id"].as_str()).collect()
    }

    #[test]
    fn no_models_allowlist_exposes_the_full_worker_catalog() {
        let all = vec![
            (0, json!({ "id": "claude/claude-haiku-5-5" })),
            (1, json!({ "id": "codex/gpt-6.1-sol" })),
            (0, json!({ "id": "faux/smoke" })),
        ];
        assert_eq!(id(&visible_models(&all, None)), ["claude/claude-haiku-5-5", "codex/gpt-6.1-sol", "faux/smoke"]);
    }

    #[test]
    fn explicit_models_allowlist_orders_and_filters_but_keeps_faux_models() {
        let all = vec![
            (0, json!({ "id": "claude/claude-haiku-5-5" })),
            (1, json!({ "id": "codex/gpt-6.1-sol" })),
            (0, json!({ "id": "faux/smoke" })),
        ];
        assert_eq!(id(&visible_models(&all, Some("codex/gpt-6.1-sol,unknown,claude/claude-haiku-5-5"))),
                   ["codex/gpt-6.1-sol", "claude/claude-haiku-5-5", "faux/smoke"]);
    }
}
