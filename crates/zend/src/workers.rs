//! Workers: starting and supervising model workers, and routing models to them
//! (docs/worker-protocol.md). System One runs in the kernel (score.rs).

use super::*;

use std::collections::HashSet;

use zen_proto::worker;

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

/// Workers to run, from ZEN_WORKERS (default: `engine`), and the names it lists that are ignored.
/// A name other than `engine` needs its command in `ZEN_WORKER_<NAME>_CMD`; one without (a retired
/// worker such as `pi`, a typo) is ignored with a warning instead of starting a worker that can't
/// run, which would fail the kernel's health check (and roll back an upgrade).
pub(crate) fn worker_configs() -> (Vec<(String, String, String)>, Vec<String>) {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
    let configured = std::env::var("ZEN_WORKERS").unwrap_or_else(|_| "engine".into());
    let (mut workers, mut ignored) = (Vec::new(), Vec::new());
    for name in configured.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        if workers.iter().any(|(n, _, _)| n == name) {
            continue;
        }
        let cmd = match name {
            "engine" => std::env::var("ZEN_ENGINE_CMD").unwrap_or_else(|_| exe_dir.join("zen-engine").display().to_string()),
            other => match std::env::var(format!("ZEN_WORKER_{}_CMD", other.to_uppercase())) {
                Ok(cmd) if !cmd.trim().is_empty() => cmd,
                _ => {
                    tracing::warn!("ZEN_WORKERS lists `{other}`, which isn't a known worker (no ZEN_WORKER_{}_CMD); ignoring it", other.to_uppercase());
                    ignored.push(other.to_string());
                    continue;
                }
            },
        };
        workers.push((name.to_string(), cmd, ".".to_string()));
    }
    if workers.is_empty() {
        let cmd = std::env::var("ZEN_ENGINE_CMD").unwrap_or_else(|_| exe_dir.join("zen-engine").display().to_string());
        workers.push(("engine".to_string(), cmd, ".".to_string()));
    }
    (workers, ignored)
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
/// summaries).
pub(crate) async fn complete(app: &App, model: &str, system: &str, prompt: &str) -> Result<worker::Completion> {
    let Some(w) = worker_for(app, model).await else { anyhow::bail!("no worker serves `{model}`") };
    let req = worker::Request::Complete(worker::Complete { model: model.into(), system: system.into(), prompt: prompt.into() });
    let res = app.workers[w].mind().call_within(&req, Duration::from_secs(600)).await?;
    if let Some(e) = res["error"].as_str() {
        anyhow::bail!("{e}");
    }
    Ok(serde_json::from_value(res)?)
}

/// The JSON object in a completion's text: from its first `{` to its last `}`, so a reply wrapped
/// in a code fence or a sentence still parses. None when there's no object there.
pub(crate) fn json_object(reply: &str) -> Option<Value> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let v: Value = serde_json::from_str(reply.get(start..=end)?).ok()?;
    v.is_object().then_some(v)
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
    let mut failed = Vec::new();
    for (i, w) in app.workers.iter().enumerate() {
        let listed = w.mind().call(&worker::Request::ModelsList {}).await.and_then(|res| Ok(serde_json::from_value::<worker::ModelsList>(res)?));
        match listed {
            Ok(res) => {
                authenticated.extend(res.authenticated);
                // The catalog keeps each model as its worker described it (JSON), for clients.
                all.extend(res.models.iter().filter_map(|m| serde_json::to_value(m).ok()).map(|m| (i, m)));
            }
            Err(e) => {
                tracing::warn!("worker `{}` didn't list its models ({e:#}); keeping its previous ones", w.name);
                failed.push(i);
            }
        }
    }
    // A worker that didn't answer this time (busy, restarting, slow) keeps the models it listed
    // before, so one slow worker doesn't take the others' models out of routing.
    if !failed.is_empty() {
        let (routes, catalog) = (app.routes.lock().await, app.catalog.lock().await);
        all.extend(kept_models(&routes, &catalog, &failed));
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

/// The models `failed` workers served before, from the current routes and catalog, by id.
fn kept_models(routes: &HashMap<String, usize>, catalog: &HashMap<String, Value>, failed: &[usize]) -> Vec<(usize, Value)> {
    let mut kept: Vec<(usize, Value)> = routes
        .iter()
        .filter(|(_, w)| failed.contains(w))
        .filter_map(|(id, w)| catalog.get(id).map(|m| (*w, m.clone())))
        .collect();
    kept.sort_by(|a, b| a.1["id"].as_str().cmp(&b.1["id"].as_str()));
    kept
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

/// The thinking level a session keeps when it moves to `model`: its own, if `model` takes it;
/// else None (the model's default).
pub(crate) async fn effort_on(app: &App, model: &str, effort: Option<String>) -> Option<String> {
    match effort {
        Some(e) if check_effort(app, model, Some(&e)).await.is_err() => None,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_object_is_found_in_prose_or_a_fence() {
        assert_eq!(json_object("Sure:\n```json\n{\"a\": 1}\n```"), Some(json!({ "a": 1 })));
        assert_eq!(json_object("} then {"), None, "a closing brace before the opening one");
        assert_eq!(json_object("[1, 2]"), None);
        assert_eq!(json_object("{not json}"), None);
    }

    fn id(models: &[Value]) -> Vec<&str> {
        models.iter().filter_map(|m| m["id"].as_str()).collect()
    }

    #[test]
    fn a_worker_that_fails_to_list_keeps_its_models() {
        let routes = HashMap::from([("claude/a".to_string(), 0), ("slow/b".to_string(), 1)]);
        let catalog = HashMap::from([("claude/a".to_string(), json!({ "id": "claude/a" })), ("slow/b".to_string(), json!({ "id": "slow/b" }))]);
        assert_eq!(kept_models(&routes, &catalog, &[1]), vec![(1, json!({ "id": "slow/b" }))]);
        assert!(kept_models(&routes, &catalog, &[]).is_empty());
    }

    #[test]
    fn unknown_worker_names_are_ignored() {
        // One test changes the process's environment, so all cases are in it.
        std::env::set_var("ZEN_WORKERS", "engine, pi ,slow,engine");
        std::env::set_var("ZEN_WORKER_SLOW_CMD", "python3 slow.py");
        let (workers, ignored) = worker_configs();
        assert_eq!(workers.iter().map(|w| w.0.as_str()).collect::<Vec<_>>(), ["engine", "slow"]);
        assert_eq!(workers[1].1, "python3 slow.py");
        assert_eq!(ignored, ["pi"]);
        std::env::set_var("ZEN_WORKERS", "pi");
        let (workers, ignored) = worker_configs();
        assert_eq!((workers[0].0.as_str(), ignored), ("engine", vec!["pi".to_string()]), "never none");
        std::env::remove_var("ZEN_WORKERS");
        std::env::remove_var("ZEN_WORKER_SLOW_CMD");
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
