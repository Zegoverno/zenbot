//! The owner's preferences, set from the web app: a key and its JSON value in the `prefs` table
//! (migration `0013_prefs.sql`). `GET /api/prefs` returns the ones that are set as one object;
//! `PATCH /api/prefs` merges: a key given is validated and replaced, `null` deletes it.
//!
//! - `name`: what the app calls the owner (a string, at most 100 characters).
//! - `home_layout`: the Home board's layout, opaque JSON to the kernel (an object or array, at
//!   most 64 KB).
//! - `default_model`: the model new sessions start on, over `ZEN_DEFAULT_MODEL`: one a worker
//!   serves right now. No restart: each new session reads it (`new_session_defaults`).
//! - `default_effort`: the thinking level new sessions start with when they get the default
//!   model: one that model takes.
//!
//! A model the kernel stops serving later (an engine signed out) is skipped when a session is
//! created, and the kernel's own default is used.

use serde_json::{json, Map, Value};
use sqlx::SqlitePool;

use crate::api::{ApiError, ApiResult};
use crate::AppState;

/// The keys a client may set.
pub const KEYS: [&str; 4] = ["name", "home_layout", "default_model", "default_effort"];

const NAME_MAX: usize = 100;
const LAYOUT_MAX: usize = 64 * 1024;

/// Every preference that is set.
pub async fn all(db: &SqlitePool) -> Result<Map<String, Value>, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM prefs").fetch_all(db).await?;
    Ok(rows.into_iter().filter_map(|(k, v)| Some((k, serde_json::from_str(&v).ok()?))).collect())
}

/// Seed the display name once from the first line of USER.md's “Who I am” section.
/// INSERT OR IGNORE protects a name the owner has already set, including concurrent edits.
pub async fn seed_name(db: &SqlitePool, user_text: &str) -> Result<(), sqlx::Error> {
    if let Some(name) = name_from_user(user_text) {
        sqlx::query("INSERT OR IGNORE INTO prefs (key, value) VALUES ('name', $1)")
            .bind(json!(name).to_string()).execute(db).await?;
    }
    Ok(())
}

fn name_from_user(text: &str) -> Option<&str> {
    let mut lines = text.lines();
    lines.find(|line| line.trim() == "## Who I am")?;
    let line = lines.map(str::trim).find(|line| !line.is_empty())?;
    if line.starts_with('#') { return None; }
    let name = line.split([',', '.', '(']).next()?.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control) { None } else { Some(name) }
}

/// One preference, when it is set.
pub async fn get(db: &SqlitePool, key: &str) -> Option<Value> {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM prefs WHERE key = $1").bind(key).fetch_optional(db).await.ok().flatten();
    v.and_then(|v| serde_json::from_str(&v).ok())
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(axum::http::StatusCode::BAD_REQUEST, msg.into())
}

/// The change `value` makes to `key`: `Some(json)` to set it, `None` to delete it.
async fn checked(app: &AppState, key: &str, value: &Value, pending: &Map<String, Value>) -> ApiResult<Option<Value>> {
    if value.is_null() {
        return Ok(None);
    }
    match key {
        "name" => {
            let n = value.as_str().ok_or_else(|| bad("`name` must be a string or null"))?.trim();
            if n.is_empty() || n.chars().count() > NAME_MAX || n.chars().any(char::is_control) {
                return Err(bad(format!("`name` must be 1 to {NAME_MAX} characters on one line")));
            }
            Ok(Some(json!(n)))
        }
        "home_layout" => {
            if !(value.is_object() || value.is_array()) {
                return Err(bad("`home_layout` must be a JSON object or array, or null"));
            }
            if value.to_string().len() > LAYOUT_MAX {
                return Err(bad(format!("`home_layout` can be {} KB at most", LAYOUT_MAX / 1024)));
            }
            Ok(Some(value.clone()))
        }
        "default_model" => {
            let m = value.as_str().ok_or_else(|| bad("`default_model` must be a string or null"))?.trim();
            if crate::workers::model_info(app, m).await.is_none() {
                return Err(bad(format!("`{m}` is not a model the kernel serves now (GET /api/models lists them)")));
            }
            Ok(Some(json!(m)))
        }
        "default_effort" => {
            let e = value.as_str().ok_or_else(|| bad("`default_effort` must be a string or null"))?.trim();
            // The level must suit the model new sessions will get: the one just set, else the stored one.
            let model = match pending.get("default_model") {
                Some(m) if !m.is_null() => m.as_str().map(String::from),
                Some(_) => None,
                None => get(&app.db, "default_model").await.and_then(|m| m.as_str().map(String::from)),
            }
            .unwrap_or_else(|| app.default_model.clone());
            crate::workers::check_effort(app, &model, Some(e)).await?;
            Ok(Some(json!(e)))
        }
        _ => Err(bad(format!("unknown preference `{key}`; the preferences are {}", KEYS.join(", ")))),
    }
}

/// `PATCH /api/prefs`: merge the keys given, return every preference that is set.
pub(crate) async fn patch(app: &AppState, body: &Value) -> ApiResult<Map<String, Value>> {
    let obj = body.as_object().ok_or_else(|| bad("the body must be a JSON object"))?;
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(bad(format!("unknown preference `{k}`; the preferences are {}", KEYS.join(", "))));
    }
    // The model first: the effort is checked against it.
    let mut changes: Vec<(&str, Option<Value>)> = Vec::new();
    let mut pending = Map::new();
    for key in KEYS {
        if let Some(v) = obj.get(key) {
            pending.insert(key.to_string(), v.clone());
        }
    }
    for key in KEYS {
        if let Some(v) = obj.get(key) {
            changes.push((key, checked(app, key, v, &pending).await?));
        }
    }
    let mut tx = app.db.begin().await?;
    for (key, value) in changes {
        match value {
            Some(v) => {
                sqlx::query("INSERT INTO prefs (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')")
                    .bind(key)
                    .bind(v.to_string())
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM prefs WHERE key = $1").bind(key).execute(&mut *tx).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(all(&app.db).await?)
}

/// Resolve a new session or job: an explicit session/job model wins over an agent model,
/// then the owner's current default, then the environment default. An explicit effort wins;
/// otherwise the owner's effort applies only when the selected model is the current global
/// default (and supports it). Child sessions keep their inherited settings instead.
pub(crate) async fn new_session_defaults(app: &AppState, agent_model: Option<String>) -> (String, Option<String>) {
    let preferred = match get(&app.db, "default_model").await.and_then(|m| m.as_str().map(String::from)) {
        Some(m) if crate::workers::model_info(app, &m).await.is_some() => m,
        _ => app.default_model.clone(),
    };
    let model = agent_model.unwrap_or_else(|| preferred.clone());
    let effort = if model == preferred {
        match get(&app.db, "default_effort").await.and_then(|e| e.as_str().map(String::from)) {
            Some(e) => crate::workers::effort_on(app, &model, Some(e)).await,
            None => None,
        }
    } else { None };
    (model, effort)
}

/// The model new sessions get now, for `GET /api/models` (`default`): the preferred one when it is
/// set, else `ZEN_DEFAULT_MODEL`'s.
pub(crate) async fn default_model(app: &crate::App) -> String {
    match get(&app.db, "default_model").await.and_then(|m| m.as_str().map(String::from)) {
        Some(m) => m,
        None => app.default_model.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The models the test kernel "serves" (it has no workers: an unknown model refreshes the
    /// catalog to nothing, so tests put it back).
    async fn seed(app: &crate::AppState) {
        app.catalog.lock().await.insert("faux/smoke".into(), json!({ "id": "faux/smoke", "efforts": ["low", "high"] }));
        app.catalog.lock().await.insert("faux/other".into(), json!({ "id": "faux/other", "efforts": [] }));
    }

    async fn app_with_models() -> (crate::test_util::TestDir, crate::AppState) {
        let (dir, db) = crate::test_util::database().await;
        let app = crate::test_util::app(db);
        seed(&app).await;
        (dir, app)
    }

    #[tokio::test]
    async fn seed_name_preserves_owner_edits() {
        let (_dir, app) = app_with_models().await;
        seed_name(&app.db, "## Who I am\n\nJosé Governo, 32, São Paulo.\n").await.unwrap();
        assert_eq!(get(&app.db, "name").await, Some(json!("José Governo")));
        patch(&app, &json!({"name": "Ze"})).await.unwrap();
        seed_name(&app.db, "## Who I am\nAnother Name, 40.\n").await.unwrap();
        assert_eq!(get(&app.db, "name").await, Some(json!("Ze")));
        assert_eq!(name_from_user("## Who I am\n\n## How I like to work"), None);
        app.db.close().await;
    }

    #[tokio::test]
    async fn preferences_merge_and_null_deletes() {
        let (_dir, app) = app_with_models().await;
        assert!(all(&app.db).await.unwrap().is_empty());
        let got = patch(&app, &json!({ "name": "  Ze ", "home_layout": { "cards": ["tasks", "routines"] } })).await.unwrap();
        assert_eq!(Value::Object(got), json!({ "name": "Ze", "home_layout": { "cards": ["tasks", "routines"] } }));
        // A merge: the layout stays when only the name changes.
        let got = patch(&app, &json!({ "name": "José" })).await.unwrap();
        assert_eq!((got["name"].as_str(), got["home_layout"]["cards"][0].as_str()), (Some("José"), Some("tasks")));
        let got = patch(&app, &json!({ "name": null })).await.unwrap();
        assert!(!got.contains_key("name") && got.contains_key("home_layout"));
        assert_eq!(patch(&app, &json!({})).await.unwrap().len(), 1);
        app.db.close().await;
    }

    #[tokio::test]
    async fn bad_values_are_refused_and_nothing_is_saved() {
        let (_dir, app) = app_with_models().await;
        for body in [
            json!({ "name": 5 }),
            json!({ "name": "" }),
            json!({ "name": "a\nb" }),
            json!({ "name": "x".repeat(101) }),
            json!({ "home_layout": "grid" }),
            json!({ "home_layout": { "x": "y".repeat(70_000) } }),
            json!({ "default_model": "claude/not-served" }),
            json!({ "default_model": 3 }),
            json!({ "default_effort": "turbo" }),
            json!({ "colour": "green" }),
            json!([1]),
        ] {
            seed(&app).await;
            assert!(patch(&app, &body).await.is_err(), "{body}");
        }
        seed(&app).await;
        // One bad key spoils the request: the good one next to it isn't saved either.
        assert!(patch(&app, &json!({ "name": "Ze", "default_effort": "turbo" })).await.is_err());
        assert!(all(&app.db).await.unwrap().is_empty());
        app.db.close().await;
    }

    #[tokio::test]
    async fn new_sessions_use_the_preferred_model_and_effort() {
        let (_dir, app) = app_with_models().await;
        assert_eq!(new_session_defaults(&app, None).await, ("faux/smoke".to_string(), None), "the kernel's default without preferences");
        patch(&app, &json!({ "default_model": "faux/smoke", "default_effort": "high" })).await.unwrap();
        assert_eq!(new_session_defaults(&app, None).await, ("faux/smoke".to_string(), Some("high".to_string())));
        assert_eq!(default_model(&app).await, "faux/smoke");
        assert_eq!(new_session_defaults(&app, Some("faux/smoke".into())).await, ("faux/smoke".into(), Some("high".into())));
        // An agent model wins; the preferred effort only follows a matching default model.
        assert_eq!(new_session_defaults(&app, Some("faux/other".into())).await, ("faux/other".to_string(), None));
        // The effort has to suit the model: `other` has none to choose.
        assert!(patch(&app, &json!({ "default_model": "faux/other" })).await.is_ok());
        assert_eq!(new_session_defaults(&app, None).await, ("faux/other".to_string(), None));
        assert!(patch(&app, &json!({ "default_effort": "low" })).await.is_err());
        assert!(patch(&app, &json!({ "default_model": "faux/smoke", "default_effort": "low" })).await.is_ok());
        // A model that is no longer served is skipped.
        app.catalog.lock().await.remove("faux/smoke");
        sqlx::query("INSERT INTO prefs (key, value) VALUES ('default_model', '\"gone/model\"') ON CONFLICT (key) DO UPDATE SET value = excluded.value").execute(&app.db).await.unwrap();
        assert_eq!(new_session_defaults(&app, None).await.0, "faux/smoke");
        patch(&app, &json!({ "default_model": null, "default_effort": null })).await.unwrap();
        app.db.close().await;
    }
}
