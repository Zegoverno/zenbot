//! Owner-only MCP form responses. Requests remain correlated with their originating call; model
//! messages never supply answers. Unsupported schemas fail before prompting, and cancelled calls
//! close their pending forms. Restart cancels unfinished requests rather than replaying tool calls.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Cancel requests left by a previous kernel process. Call once after database migrations.
pub(crate) async fn recover(db: &PgPool) -> Result<()> {
    sqlx::query("UPDATE mcp_interactions SET status='cancel' WHERE status='pending'").execute(db).await?;
    Ok(())
}

fn supported(schema: &Value) -> Result<()> {
    if schema["type"] != "object" { bail!("elicitation requires an object schema"); }
    let root = schema.as_object().context("invalid elicitation schema")?;
    if root.keys().any(|k| !matches!(k.as_str(), "type"|"title"|"description"|"properties"|"required"|"additionalProperties")) {
        bail!("unsupported elicitation object schema keyword");
    }
    let props = schema["properties"].as_object().context("elicitation properties must be an object")?;
    if props.len() > 30 { bail!("elicitation exceeds 30 fields"); }
    if schema.get("additionalProperties").is_some_and(|v| v != false) { bail!("elicitation requires closed properties"); }
    if let Some(required) = schema.get("required") {
        let required = required.as_array().context("invalid required fields")?;
        if required.iter().any(|v| v.as_str().is_none_or(|k| !props.contains_key(k))) { bail!("unknown required field"); }
    }
    for field in props.values() {
        let f = field.as_object().context("invalid elicitation field")?;
        if f.keys().any(|k| !matches!(k.as_str(), "type"|"title"|"description"|"enum"|"enumNames"|"default"|"minimum"|"maximum"|"minLength"|"maxLength")) {
            bail!("unsupported elicitation field schema keyword");
        }
        if !matches!(field["type"].as_str(), Some("string"|"boolean"|"number"|"integer")) { bail!("unsupported elicitation field type"); }
        for key in ["minimum", "maximum"] {
            if field.get(key).is_some_and(|v| v.as_f64().is_none()) { bail!("invalid numeric bound"); }
        }
        for key in ["minLength", "maxLength"] {
            if field.get(key).is_some_and(|v| v.as_u64().is_none()) { bail!("invalid string bound"); }
        }
        if let Some(values) = field.get("enum") {
            if !values.as_array().is_some_and(|a| !a.is_empty() && a.iter().all(|v| valid_type(field, v))) { bail!("invalid elicitation enum"); }
        }
    }
    Ok(())
}

fn valid_type(field: &Value, value: &Value) -> bool {
    match field["type"].as_str() {
        Some("string") => value.is_string(), Some("boolean") => value.is_boolean(),
        Some("integer") => value.is_i64() || value.is_u64(), Some("number") => value.is_number(), _ => false,
    }
}

fn validate(schema: &Value, content: &Value) -> Result<()> {
    supported(schema)?;
    let props = schema["properties"].as_object().unwrap();
    let data = content.as_object().context("accepted content must be an object")?;
    if data.len() > 30 || serde_json::to_vec(content)?.len() > 32 * 1024 { bail!("elicitation response exceeds size limit"); }
    if schema["required"].as_array().is_some_and(|a| a.iter().any(|v| !data.contains_key(v.as_str().unwrap()))) { bail!("missing required field"); }
    for (name, value) in data {
        let field = props.get(name).context("unknown response field")?;
        if !valid_type(field, value) { bail!("wrong type for {name}"); }
        if field["enum"].as_array().is_some_and(|a| !a.contains(value)) { bail!("invalid choice for {name}"); }
        if let Some(s) = value.as_str() {
            let len = s.chars().count() as u64;
            if field["minLength"].as_u64().is_some_and(|n| len < n) || field["maxLength"].as_u64().is_some_and(|n| len > n) { bail!("invalid length for {name}"); }
        }
        if let Some(n) = value.as_f64() {
            if field["minimum"].as_f64().is_some_and(|min| n < min) || field["maximum"].as_f64().is_some_and(|max| n > max) { bail!("out of range for {name}"); }
        }
    }
    Ok(())
}

struct Pending { db: PgPool, id: Uuid }
impl Drop for Pending {
    fn drop(&mut self) {
        let db = self.db.clone(); let id = self.id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = sqlx::query("UPDATE mcp_interactions SET status='cancel' WHERE id=$1 AND status='pending'").bind(id).execute(&db).await;
            });
        }
    }
}

pub(crate) async fn elicit(app: &crate::App, session: Uuid, server: &str, request_id: &Value, params: &Value) -> Result<Value> {
    let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM sessions WHERE id=$1")
        .bind(session).fetch_one(&app.db).await?;
    if let Some(why) = crate::agent::refusal(kind.as_deref(), "ask") { bail!("MCP elicitation refused: {why}"); }
    if params.get("mode").is_some_and(|v| v != "form") { bail!("only form elicitation is supported"); }
    if !(request_id.is_string() || request_id.is_i64() || request_id.is_u64()) || request_id.as_str().is_some_and(|s| s.len()>1024) {
        bail!("invalid MCP request id");
    }
    let schema = &params["requestedSchema"];
    supported(schema)?;
    let message = params["message"].as_str().context("elicitation requires a message")?;
    if message.len() > 8 * 1024 || serde_json::to_vec(schema)?.len() > 32 * 1024 { bail!("elicitation request exceeds size limit"); }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_interactions(id,session_id,server,request_id,message,schema) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(session).bind(server).bind(request_id).bind(crate::secrets::mask(message)).bind(schema).execute(&app.db).await?;
    let _pending = Pending { db: app.db.clone(), id };
    let question = json!({"mcp_request":id,"questions":[{"question":format!("MCP server {server} requests input: {}. Review `zen mcp-pending`; reply with `zen mcp-respond {id} --action accept --data '{{...}}'`, or --action decline/cancel.", crate::secrets::mask(message)),"options":["Review pending MCP form","Decline or cancel via zen mcp-respond"]}]});
    crate::tape::append(&app.db, session, "questions", &question).await?;
    app.emit(session, json!({"type":"questions","questions":question["questions"]})).await;
    loop {
        sqlx::query("UPDATE mcp_interactions SET status='cancel' WHERE id=$1 AND status='pending' AND expires_at<=now()").bind(id).execute(&app.db).await?;
        let row = sqlx::query("SELECT status,content FROM mcp_interactions WHERE id=$1").bind(id).fetch_one(&app.db).await?;
        let action: String = row.get("status");
        if action != "pending" {
            return Ok(if action == "accept" {json!({"action":action,"content":row.get::<Option<Value>,_>("content")})} else {json!({"action":action})});
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

pub(crate) async fn list(db: &PgPool) -> Result<Value> {
    sqlx::query("UPDATE mcp_interactions SET status='cancel' WHERE status='pending' AND expires_at<=now()").execute(db).await?;
    let rows = sqlx::query("SELECT id,session_id,server,message,schema,created_at,expires_at FROM mcp_interactions WHERE status='pending' ORDER BY created_at LIMIT 100").fetch_all(db).await?;
    Ok(json!({"requests":rows.iter().map(|r| json!({"id":r.get::<Uuid,_>("id"),"session":r.get::<Uuid,_>("session_id"),"server":r.get::<String,_>("server"),"message":r.get::<String,_>("message"),"schema":r.get::<Value,_>("schema"),"created_at":r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),"expires_at":r.get::<chrono::DateTime<chrono::Utc>,_>("expires_at")})).collect::<Vec<_>>()}))
}

pub(crate) async fn respond(db: &PgPool, id: Uuid, response: &Value) -> Result<Value> {
    let action = response["action"].as_str().context("action required")?;
    if !matches!(action,"accept"|"decline"|"cancel") { bail!("action must be accept, decline or cancel"); }
    let schema: Value = sqlx::query_scalar("SELECT schema FROM mcp_interactions WHERE id=$1 AND status='pending' AND expires_at>now()")
        .bind(id).fetch_optional(db).await?.context("request is absent, expired or already answered")?;
    if action == "accept" { validate(&schema, &response["content"])?; }
    else if response.get("content").is_some() { bail!("content is only valid with accept"); }
    let result = sqlx::query("UPDATE mcp_interactions SET status=$2,content=$3 WHERE id=$1 AND status='pending' AND expires_at>now()")
        .bind(id).bind(action).bind(if action == "accept" {Some(&response["content"])} else {None}).execute(db).await?;
    if result.rows_affected() != 1 { bail!("request expired or was already answered"); }
    Ok(json!({"id":id,"action":action}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "SOURCE_TEST_DB_URL must name a disposable database"]
    async fn owner_response_lifecycle_database() {
        let db = PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        let app = crate::test_util::app(db.clone());
        let session = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model) VALUES($1,'faux/smoke')").bind(session).execute(&db).await.unwrap();
        let spawn = |app: crate::AppState| tokio::spawn(async move {
            elicit(&app,session,"fixture",&json!(3),&json!({"message":"Choose","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}})).await
        });
        let pending_id = || async {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if let Some(id) = sqlx::query_scalar::<_,Uuid>("SELECT id FROM mcp_interactions WHERE session_id=$1 AND status='pending'").bind(session).fetch_optional(&db).await.unwrap() { break id; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.unwrap()
        };
        let task=spawn(app.clone()); let id=pending_id().await;
        assert!(respond(&db,id,&json!({"action":"accept","content":{}})).await.is_err());
        respond(&db,id,&json!({"action":"accept","content":{"name":"Jo"}})).await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(),json!({"action":"accept","content":{"name":"Jo"}}));
        assert!(respond(&db,id,&json!({"action":"cancel"})).await.is_err());
        let task=spawn(app.clone()); let id=pending_id().await;
        sqlx::query("UPDATE mcp_interactions SET expires_at=now()-interval '1 second' WHERE id=$1").bind(id).execute(&db).await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(),json!({"action":"cancel"}));
        let task=spawn(app.clone()); let id=pending_id().await; task.abort(); let _=task.await;
        tokio::time::timeout(std::time::Duration::from_secs(3),async {
            loop {
                let status:String=sqlx::query_scalar("SELECT status FROM mcp_interactions WHERE id=$1").bind(id).fetch_one(&db).await.unwrap();
                if status=="cancel" {break;}
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let task=spawn(app); let _id=pending_id().await; recover(&db).await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(),json!({"action":"cancel"}));
    }

    #[test]
    fn form_validation_enforces_types_bounds_choices_and_required() {
        let schema=json!({"type":"object","properties":{"name":{"type":"string","minLength":2},"count":{"type":"integer","minimum":1,"maximum":3},"ok":{"type":"boolean"},"choice":{"type":"string","enum":["a","b"]}},"required":["name"]});
        assert!(validate(&schema,&json!({"name":"Jo","count":2,"ok":true,"choice":"a"})).is_ok());
        for bad in [json!({}),json!({"name":"x"}),json!({"name":"Jo","count":4}),json!({"name":"Jo","count":1.5}),json!({"name":"Jo","choice":"c"}),json!({"name":"Jo","unknown":true})] { assert!(validate(&schema,&bad).is_err(),"{bad}"); }
        assert!(supported(&json!({"type":"object","properties":{"nested":{"type":"object"}}})).is_err());
        assert!(supported(&json!({"type":"object","properties":{"secret":{"type":"string","format":"password"}}})).is_err());
    }
}
