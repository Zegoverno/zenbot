//! Minimal work contracts: immutable criteria, one session-bound attempt and submitted artifacts.
//! The tape is authoritative; work_* tables are query projections, committed with each event.
//! Submission means execution completed, NOT verified or accepted. No routing/retries/assessment
//! yet. Startup interrupts unfinished attempts before workers start; recovery never runs live.
//! Files are hashed through checked descriptors; hashes establish identity, not correctness.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;
use crate::{agents, tape};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Criterion {
    id: String,
    text: String,
    method: Method,
    #[serde(skip_serializing_if="Option::is_none")]
    run: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]
    expect: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all="snake_case")]
enum Method { Command, Review }
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Contract { goal: String, criteria: Vec<Criterion> }

#[derive(Deserialize, Serialize)]
#[serde(tag="action", rename_all="snake_case", deny_unknown_fields)]
enum Request {
    Create { key: String, contract: Contract, vault_task_id: Option<String> },
    Get { work_id: Uuid },
    Submit { key: String, attempt_id: Uuid, artifacts: Vec<ArtifactInput> },
}
#[derive(Deserialize, Serialize)]
#[serde(tag="kind", rename_all="snake_case", deny_unknown_fields)]
enum ArtifactInput { Answer { seq: i32 }, File { path: String } }

fn text(value: &str, max: usize, field: &str) -> Result<()> {
    ensure!(!value.trim().is_empty() && value.len()<=max && !value.contains('\0'), "{field} must be nonempty, NUL-free and at most {max} bytes");
    Ok(())
}
fn validate(contract: &Contract) -> Result<()> {
    text(&contract.goal, 8192, "goal")?;
    ensure!((1..=32).contains(&contract.criteria.len()), "need 1 to 32 criteria");
    let mut ids=HashSet::new();
    for c in &contract.criteria {
        text(&c.id,64,"criterion id")?;
        ensure!(ids.insert(&c.id), "duplicate criterion id");
        text(&c.text,4096,"criterion text")?;
        match c.method {
            Method::Command => {
                text(c.run.as_deref().context("command criterion needs run")?,8192,"run")?;
                if let Some(e)=&c.expect { text(e,4096,"expect")?; }
            }
            Method::Review => ensure!(c.run.is_none() && c.expect.is_none(), "review criterion cannot have run/expect"),
        }
    }
    Ok(())
}
fn digest(bytes: &[u8]) -> String { format!("{:x}",Sha256::digest(bytes)) }

pub fn spec() -> Value {
    json!({"name":"work","description":"Create immutable work and start an attempt; get its history; submit artifacts to end execution, NOT verify or accept. Write keys are idempotent; changed inputs conflict.",
    "parameters":{"type":"object","properties":{
        "action":{"type":"string","enum":["create","get","submit"]},
        "key":{"type":"string"},
        "contract":{"type":"object","properties":{"goal":{"type":"string"},"criteria":{"type":"array","items":{"type":"object","properties":{
            "id":{"type":"string"},"text":{"type":"string"},"method":{"type":"string","enum":["command","review"]},
            "run":{"type":"string"},"expect":{"type":"string"}},"required":["id","text","method"],"additionalProperties":false}}},
            "required":["goal","criteria"],"additionalProperties":false},
        "vault_task_id":{"type":"string"},
        "work_id":{"type":"string"},
        "attempt_id":{"type":"string"},
        "artifacts":{"type":"array","items":{"type":"object","properties":{
            "kind":{"type":"string","enum":["answer","file"]},"seq":{"type":"integer"},
            "path":{"type":"string","description":"Workspace-relative regular file"}},"required":["kind"]}}
    },"required":["action"],"additionalProperties":false}})
}

pub async fn run(db: &SqlitePool, session: Uuid, who: &agents::Agent, workspace: &Path, args: &Value) -> Result<Value> {
    ensure!(args.to_string().len()<=128*1024,"work request exceeds 128 KB");
    let request: Request=serde_json::from_value(args.clone()).context("invalid work request")?;
    execute(db,session,&who.name,workspace,who.sandbox().as_ref(),request).await
}

async fn execute(db: &SqlitePool, session: Uuid, agent: &str, workspace: &Path, sandbox: Option<&crate::sandbox::Sandbox>, request: Request) -> Result<Value> {
    let canonical=serde_json::to_string(&request)?;
    // Authorize before touching files, without holding SQLite's writer lock across I/O.
    // Recheck the same binding/status inside the write transaction before committing.
    let mut files=std::collections::HashMap::new();
    if let Request::Submit {attempt_id,artifacts,..}=&request {
        let status: Option<String>=sqlx::query_scalar("SELECT a.status FROM work_attempts a JOIN work_items w ON w.id=a.work_id JOIN sessions s ON s.id=a.session_id WHERE a.id=$1 AND a.session_id=$2 AND w.agent=$3 AND s.agent=$3")
            .bind(attempt_id).bind(session).bind(agent).fetch_optional(db).await?;
        ensure!(status.is_some(),"attempt not found");
        if status.as_deref()==Some("running") {
            ensure!((1..=32).contains(&artifacts.len()),"submit needs 1 to 32 artifacts");
            for artifact in artifacts {
                if let ArtifactInput::File {path}=artifact {
                    let root=workspace.to_path_buf();
                    let sandbox=sandbox.cloned();
                    let path=path.clone();
                    let key=path.clone();
                    let identity=tokio::task::spawn_blocking(move || file_identity(&root,&path,sandbox.as_ref())).await??;
                    files.insert(key,identity);
                }
            }
        }
    }
    let mut tx=if matches!(request,Request::Get {..}) {db.begin().await?} else {crate::storage::begin(db).await?};
    let actual: String=sqlx::query_scalar("SELECT agent FROM sessions WHERE id=$1").bind(session).fetch_one(&mut *tx).await?;
    ensure!(actual==agent,"session agent mismatch");
    match request {
        Request::Create {key,contract,vault_task_id} => {
            text(&key,128,"key")?;
            validate(&contract)?;
            if let Some(id)=&vault_task_id {text(id,128,"vault task id")?;}
            if let Some(row)=sqlx::query("SELECT id,request FROM work_items WHERE session_id=$1 AND request_key=$2")
                .bind(session).bind(&key).fetch_optional(&mut *tx).await? {
                ensure!(row.get::<String,_>("request")==canonical,"idempotency key already used with different input");
                return get_in(&mut tx,row.get("id"),agent).await;
            }
            let id=Uuid::new_v4();
            let attempt=Uuid::new_v4();
            let contract=serde_json::to_value(contract)?;
            let (start_seq, _) = tape::append_in(&mut tx,session,"work_created",&json!({"work_id":id,"attempt_id":attempt,"session_id":session,"agent":agent,"key":key,"contract":contract,"vault_task_id":vault_task_id,"request":serde_json::from_str::<Value>(&canonical)?})).await?;
            sqlx::query("INSERT INTO work_items(id,session_id,agent,request_key,request,contract,vault_task_id) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(id).bind(session).bind(agent).bind(key).bind(canonical).bind(contract.to_string()).bind(vault_task_id).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO work_attempts(id,work_id,session_id,status,start_seq) VALUES($1,$2,$3,'running',$4)")
                .bind(attempt).bind(id).bind(session).bind(start_seq).execute(&mut *tx).await?;
            let result=get_in(&mut tx,id,agent).await?;
            tx.commit().await?;
            Ok(result)
        }
        Request::Get {work_id} => get_in(&mut tx,work_id,agent).await,
        Request::Submit {key,attempt_id,artifacts} => {
            text(&key,128,"key")?;
            ensure!((1..=32).contains(&artifacts.len()),"submit needs 1 to 32 artifacts");
            let row=sqlx::query("SELECT a.*, w.agent FROM work_attempts a JOIN work_items w ON w.id=a.work_id WHERE a.id=$1 AND a.session_id=$2 AND w.agent=$3")
                .bind(attempt_id).bind(session).bind(agent).fetch_optional(&mut *tx).await?.context("attempt not found")?;
            let work_id: Uuid=row.get("work_id");
            if let Some(previous)=row.get::<Option<String>,_>("submission_key") {
                ensure!(previous==key && row.get::<Option<String>,_>("submission").as_deref()==Some(&canonical),"attempt already submitted with different input");
                return get_in(&mut tx,work_id,agent).await;
            }
            ensure!(row.get::<String,_>("status")=="running","attempt is not running");
            let mut records=Vec::new();
            let mut seen=HashSet::new();
            for input in artifacts {
                let (kind,reference,sha)=match input {
                    ArtifactInput::Answer {seq} => {
                        ensure!(seq>row.get::<i32,_>("start_seq"),"answer must follow attempt creation");
                        let value: Value=sqlx::query_scalar("SELECT payload FROM tape_events WHERE session_id=$1 AND seq=$2 AND kind='message'")
                            .bind(session).bind(seq).fetch_optional(&mut *tx).await?.context("answer block not found")?;
                        ensure!(value["role"]=="assistant","answer must reference an assistant message");
                        let content=&value["content"];
                        let has_text=content.as_str().is_some_and(|s|!s.trim().is_empty())
                            || content.as_array().is_some_and(|blocks|blocks.iter().any(|b|b["type"]=="text" && b["text"].as_str().is_some_and(|s|!s.trim().is_empty())));
                        ensure!(has_text,"answer must contain assistant text, not only tool calls");
                        ("answer",json!({"session_id":session,"seq":seq}),digest(value.to_string().as_bytes()))
                    }
                    ArtifactInput::File {path} => {
                        let (reference,sha)=files.get(&path).cloned().context("file identity unavailable")?;
                        ("file",reference,sha)
                    }
                };
                ensure!(seen.insert(format!("{kind}:{reference}")),"duplicate artifact");
                records.push(json!({"id":Uuid::new_v4(),"kind":kind,"reference":reference,"sha256":sha}));
            }
            tape::append_in(&mut tx,session,"work_submitted",&json!({"work_id":work_id,"attempt_id":attempt_id,"key":key,"artifacts":records,"request":serde_json::from_str::<Value>(&canonical)?})).await?;
            for record in &records {
                sqlx::query("INSERT INTO work_artifacts(id,attempt_id,kind,reference,sha256) VALUES($1,$2,$3,$4,$5)")
                    .bind(record["id"].as_str().and_then(|v|Uuid::parse_str(v).ok()).context("invalid artifact id")?)
                    .bind(attempt_id).bind(record["kind"].as_str()).bind(record["reference"].to_string()).bind(record["sha256"].as_str()).execute(&mut *tx).await?;
            }
            sqlx::query("UPDATE work_attempts SET status='completed',ended_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),submission_key=$2,submission=$3 WHERE id=$1")
                .bind(attempt_id).bind(key).bind(canonical).execute(&mut *tx).await?;
            let result=get_in(&mut tx,work_id,agent).await?;
            tx.commit().await?;
            Ok(result)
        }
    }
}

async fn get_in(tx: &mut sqlx::Transaction<'_,sqlx::Sqlite>, id: Uuid, agent: &str) -> Result<Value> {
    let row=sqlx::query("SELECT * FROM work_items WHERE id=$1 AND (agent=$2 OR $2='zenbot')")
        .bind(id).bind(agent).fetch_optional(&mut **tx).await?.context("work not found")?;
    let rows=sqlx::query("SELECT * FROM work_attempts WHERE work_id=$1 ORDER BY started_at,id").bind(id).fetch_all(&mut **tx).await?;
    let mut attempts=Vec::new();
    for a in rows {
        let aid: Uuid=a.get("id");
        let artifacts=sqlx::query("SELECT * FROM work_artifacts WHERE attempt_id=$1 ORDER BY id").bind(aid).fetch_all(&mut **tx).await?
            .into_iter().map(|r|json!({"id":r.get::<Uuid,_>("id"),"kind":r.get::<String,_>("kind"),"reference":r.get::<Value,_>("reference"),"sha256":r.get::<String,_>("sha256")})).collect::<Vec<_>>();
        attempts.push(json!({"id":aid,"session_id":a.get::<Uuid,_>("session_id"),"execution":a.get::<String,_>("status"),"started_at":a.get::<String,_>("started_at"),"ended_at":a.get::<Option<String>,_>("ended_at"),"artifacts":artifacts,"verification":"unassessed","acceptance":"pending"}));
    }
    Ok(json!({"id":id,"session_id":row.get::<Uuid,_>("session_id"),"agent":row.get::<String,_>("agent"),"contract":row.get::<Value,_>("contract"),"vault_task_id":row.get::<Option<String>,_>("vault_task_id"),"attempts":attempts}))
}

fn file_identity(workspace: &Path, path: &str, sandbox: Option<&crate::sandbox::Sandbox>) -> Result<(Value,String)> {
    text(path,4096,"artifact path")?;
    ensure!(Path::new(path).components().all(|c|matches!(c,Component::Normal(_))),"artifact path must be workspace-relative without traversal");
    let root=std::fs::canonicalize(workspace)?;
    let named=root.join(path);
    let mut file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK).open(&named)?;
    let real=std::fs::read_link(format!("/proc/self/fd/{}",std::os::fd::AsRawFd::as_raw_fd(&file)))?;
    ensure!(real.starts_with(&root),"artifact outside workspace");
    if let Some(sandbox)=sandbox {sandbox.check(&real,false).map_err(anyhow::Error::msg)?;}
    let before=file.metadata()?;
    ensure!(before.is_file() && before.len()<=16*1024*1024,"artifact must be a regular file at most 16 MB");
    let mut bytes=Vec::new();
    (&mut file).take(16*1024*1024+1).read_to_end(&mut bytes)?;
    ensure!(bytes.len()<=16*1024*1024,"artifact exceeds 16 MB");
    let after=file.metadata()?;
    ensure!(before.len()==after.len() && before.modified()?==after.modified()?,"artifact changed while hashing");
    Ok((json!({"path":path,"workspace":root,"bytes":bytes.len()}),digest(&bytes)))
}

/// Call once at startup, after tape repair and before workers/sessions are accepted.
pub async fn recover(db: &SqlitePool) -> Result<u64> {
    let mut tx=crate::storage::begin(db).await?;
    let rows=sqlx::query("SELECT id,work_id,session_id FROM work_attempts WHERE status='running'").fetch_all(&mut *tx).await?;
    for row in &rows {
        let id: Uuid=row.get("id");
        tape::append_in(&mut tx,row.get("session_id"),"work_interrupted",&json!({"attempt_id":id,"work_id":row.get::<Uuid,_>("work_id"),"reason":"kernel_restart"})).await?;
        sqlx::query("UPDATE work_attempts SET status='interrupted',ended_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id=$1").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn session(db: &SqlitePool, agent: &str) -> Uuid {
        let id=Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model,agent) VALUES($1,'faux/smoke',$2)")
            .bind(id).bind(agent).execute(db).await.unwrap();
        id
    }
    fn create(key: &str) -> Value {
        json!({"action":"create","key":key,"contract":{"goal":"Solve the problem","criteria":[{"id":"c1","text":"Answer is correct","method":"review"}]}})
    }
    async fn call(db: &SqlitePool, s: Uuid, agent: &str, args: Value) -> Result<Value> {
        execute(db,s,agent,Path::new("/tmp"),None,serde_json::from_value(args)?).await
    }
    async fn count(db: &SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}")).fetch_one(db).await.unwrap()
    }

    #[tokio::test]
    async fn validates_contracts_and_rejects_unknown_fields() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        for bad in [
            json!({"goal":"","criteria":[]}),
            json!({"goal":"g","criteria":[{"id":"c","text":"t","method":"command"}]}),
            json!({"goal":"g","criteria":[{"id":"c","text":"t","method":"review","run":"true"}]}),
            json!({"goal":"g","criteria":[{"id":"c","text":"t","method":"review"},{"id":"c","text":"t","method":"review"}]}),
            json!({"goal":"g","criteria":[{"id":"c","text":"t","method":"review","extra":true}]}),
            json!({"goal":"g","criteria":[{"id":"c","text":"t","method":"command","run":"true","expect":""}]}),
        ] {
            let mut req=create("bad"); req["contract"]=bad;
            assert!(call(&db,s,"zenbot",req).await.is_err());
        }
        let mut req=create("bad");req["key"]=json!(" ");
        assert!(call(&db,s,"zenbot",req).await.is_err());
        let mut req=create("bad");req["extra"]=json!(true);
        assert!(call(&db,s,"zenbot",req).await.is_err());
        assert_eq!(count(&db,"work_items").await,0);
        assert_eq!(count(&db,"tape_events").await,0);
    }

    #[tokio::test]
    async fn create_is_idempotent_immutable_and_session_scoped() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        let a=call(&db,s,"zenbot",create("one")).await.unwrap();
        assert_eq!(a,call(&db,s,"zenbot",create("one")).await.unwrap());
        assert_eq!(count(&db,"tape_events").await,1);
        let mut changed=create("one");changed["contract"]["goal"]=json!("Changed");
        assert!(call(&db,s,"zenbot",changed).await.is_err());
        assert!(call(&db,s,"zenbot",json!({"action":"update","work_id":a["id"]})).await.is_err());
        let other=session(&db,"zenbot").await;
        let b=call(&db,other,"zenbot",create("one")).await.unwrap();
        assert_ne!(a["id"],b["id"]);
        assert_eq!(a["attempts"][0]["verification"],"unassessed");
        assert_eq!(a["attempts"][0]["acceptance"],"pending");
    }

    #[tokio::test]
    async fn access_is_agent_scoped_and_writes_are_session_bound() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"alice").await;
        let a=call(&db,s,"alice",create("one")).await.unwrap();
        let bob=session(&db,"bob").await;
        let zen=session(&db,"zenbot").await;
        let get=json!({"action":"get","work_id":a["id"]});
        assert!(call(&db,bob,"bob",get.clone()).await.is_err());
        assert!(call(&db,zen,"zenbot",get).await.is_ok());
        assert!(call(&db,s,"bob",create("spoof")).await.is_err());
        for (caller,agent) in [(bob,"bob"),(zen,"zenbot"),(session(&db,"alice").await,"alice")] {
            assert!(call(&db,caller,agent,json!({"action":"submit","key":"x","attempt_id":a["attempts"][0]["id"],"artifacts":[{"kind":"answer","seq":1}]})).await.is_err());
        }
    }

    #[tokio::test]
    async fn answer_submission_is_bound_idempotent_and_not_verification() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        let (old,_)=tape::append(&db,s,"message",&json!({"role":"assistant","content":"old"})).await.unwrap();
        let a=call(&db,s,"zenbot",create("one")).await.unwrap();
        let req=|seq|json!({"action":"submit","key":"submit","attempt_id":a["attempts"][0]["id"],"artifacts":[{"kind":"answer","seq":seq}]});
        assert!(call(&db,s,"zenbot",req(old)).await.is_err());
        let (user,_)=tape::append(&db,s,"message",&json!({"role":"user","content":"not an answer"})).await.unwrap();
        assert!(call(&db,s,"zenbot",req(user)).await.is_err());
        let payload=json!({"role":"assistant","content":"result"});
        let (seq,_)=tape::append(&db,s,"message",&payload).await.unwrap();
        let done=call(&db,s,"zenbot",req(seq)).await.unwrap();
        assert_eq!(done["attempts"][0]["execution"],"completed");
        assert_eq!(done["attempts"][0]["verification"],"unassessed");
        assert_eq!(done["attempts"][0]["artifacts"][0]["sha256"],digest(payload.to_string().as_bytes()));
        let n=count(&db,"tape_events").await;
        assert_eq!(done,call(&db,s,"zenbot",req(seq)).await.unwrap());
        assert_eq!(count(&db,"tape_events").await,n);
        let mut changed=req(seq);changed["key"]=json!("different");
        assert!(call(&db,s,"zenbot",changed).await.is_err());
        assert_eq!(recover(&db).await.unwrap(),0);
    }

    #[tokio::test]
    async fn projection_failure_rolls_back_event_and_all_records() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        sqlx::query("CREATE TRIGGER reject_attempt BEFORE INSERT ON work_attempts BEGIN SELECT RAISE(ABORT,'injected'); END").execute(&db).await.unwrap();
        assert!(call(&db,s,"zenbot",create("one")).await.is_err());
        assert_eq!(count(&db,"work_items").await,0);
        assert_eq!(count(&db,"work_attempts").await,0);
        assert_eq!(count(&db,"tape_events").await,0);
        assert_eq!(count(&db,"tape_exports").await,0);
    }

    #[tokio::test]
    async fn tape_failure_rolls_back_projection_and_submission() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        let a=call(&db,s,"zenbot",create("one")).await.unwrap();
        let (seq,_)=tape::append(&db,s,"message",&json!({"role":"assistant","content":"result"})).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_event BEFORE INSERT ON tape_events WHEN NEW.kind='work_submitted' BEGIN SELECT RAISE(ABORT,'injected'); END").execute(&db).await.unwrap();
        assert!(call(&db,s,"zenbot",json!({"action":"submit","key":"x","attempt_id":a["attempts"][0]["id"],"artifacts":[{"kind":"answer","seq":seq}]})).await.is_err());
        assert_eq!(count(&db,"work_artifacts").await,0);
        let status: String=sqlx::query_scalar("SELECT status FROM work_attempts").fetch_one(&db).await.unwrap();
        assert_eq!(status,"running");
    }

    #[tokio::test]
    async fn startup_recovery_interrupts_once_and_persists_across_reopen() {
        let (dir,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        let a=call(&db,s,"zenbot",create("one")).await.unwrap();
        db.close().await;
        let db=crate::storage::connect(&format!("sqlite:{}",dir.join("test.sqlite").display())).await.unwrap();
        assert_eq!(recover(&db).await.unwrap(),1);
        assert_eq!(recover(&db).await.unwrap(),0);
        let result=call(&db,s,"zenbot",json!({"action":"get","work_id":a["id"]})).await.unwrap();
        assert_eq!(result["attempts"][0]["execution"],"interrupted");
        assert!(result["attempts"][0]["ended_at"].is_string());
        let events=tape::load(&db,s,&["work_interrupted"]).await.unwrap();
        assert_eq!(events.len(),1);
        assert_eq!(events[0].payload["attempt_id"],a["attempts"][0]["id"]);
    }

    #[test]
    fn file_identity_rejects_traversal_links_directories_and_sandbox_escape() {
        let root=crate::test_util::TestDir::new("work-artifacts");
        std::fs::write(root.join("answer.txt"),b"result").unwrap();
        let (reference,hash)=file_identity(&root,"answer.txt",None).unwrap();
        assert_eq!(hash,digest(b"result"));assert_eq!(reference["bytes"],6);
        for path in ["../answer.txt","/etc/passwd",".","missing"] {
            assert!(file_identity(&root,path,None).is_err(),"{path}");
        }
        std::os::unix::fs::symlink(root.join("answer.txt"),root.join("link")).unwrap();
        assert!(file_identity(&root,"link",None).is_err());
        std::os::unix::fs::symlink("/etc",root.join("outside")).unwrap();
        assert!(file_identity(&root,"outside/passwd",None).is_err());
        std::fs::create_dir(root.join("folder")).unwrap();
        assert!(file_identity(&root,"folder",None).is_err());
        let sandbox=crate::sandbox::Sandbox::new("limited",vec![]);
        assert!(file_identity(&root,"answer.txt",Some(&sandbox)).is_err());
        std::fs::write(root.join("answer.txt"),b"changed").unwrap();
        assert_ne!(hash,file_identity(&root,"answer.txt",None).unwrap().1);
    }

    #[tokio::test]
    async fn concurrent_create_has_one_attempt_and_event() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        let mut calls=tokio::task::JoinSet::new();
        for _ in 0..8 {
            let db=db.clone();
            calls.spawn(async move {call(&db,s,"zenbot",create("same")).await.unwrap()});
        }
        let first=calls.join_next().await.unwrap().unwrap();
        while let Some(result)=calls.join_next().await {assert_eq!(result.unwrap(),first);}
        assert_eq!(count(&db,"work_items").await,1);
        assert_eq!(count(&db,"work_attempts").await,1);
        assert_eq!(count(&db,"tape_events").await,1);
    }

    #[tokio::test]
    async fn file_submit_and_duplicate_rejection_are_atomic() {
        let (dir,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        std::fs::write(dir.join("result"),b"result").unwrap();
        let a=call(&db,s,"zenbot",create("file")).await.unwrap();
        let req=|artifacts: Value|json!({"action":"submit","key":"file","attempt_id":a["attempts"][0]["id"],"artifacts":artifacts});
        let duplicate=req(json!([{"kind":"file","path":"result"},{"kind":"file","path":"result"}]));
        assert!(execute(&db,s,"zenbot",&dir,None,serde_json::from_value(duplicate).unwrap()).await.is_err());
        assert_eq!(count(&db,"work_artifacts").await,0);
        let valid=req(json!([{"kind":"file","path":"result"}]));
        let done=execute(&db,s,"zenbot",&dir,None,serde_json::from_value(valid).unwrap()).await.unwrap();
        assert_eq!(done["attempts"][0]["artifacts"][0]["sha256"],digest(b"result"));
        assert_eq!(done["attempts"][0]["execution"],"completed");
    }

    #[tokio::test]
    async fn failed_artifact_projection_rolls_back_submission_event() {
        let (dir,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        std::fs::write(dir.join("result"),b"result").unwrap();
        let a=call(&db,s,"zenbot",create("one")).await.unwrap();
        let before=count(&db,"tape_events").await;
        sqlx::query("CREATE TRIGGER reject_artifact BEFORE INSERT ON work_artifacts BEGIN SELECT RAISE(ABORT,'injected'); END").execute(&db).await.unwrap();
        let req=json!({"action":"submit","key":"x","attempt_id":a["attempts"][0]["id"],"artifacts":[{"kind":"file","path":"result"}]});
        assert!(execute(&db,s,"zenbot",&dir,None,serde_json::from_value(req).unwrap()).await.is_err());
        assert_eq!(count(&db,"tape_events").await,before);
        assert_eq!(count(&db,"work_artifacts").await,0);
        let status: String=sqlx::query_scalar("SELECT status FROM work_attempts").fetch_one(&db).await.unwrap();
        assert_eq!(status,"running");
    }

    #[tokio::test]
    async fn concurrent_submit_has_one_transition_and_event() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        let a=call(&db,s,"zenbot",create("one")).await.unwrap();
        let (seq,_)=tape::append(&db,s,"message",&json!({"role":"assistant","content":"result"})).await.unwrap();
        let req=json!({"action":"submit","key":"same","attempt_id":a["attempts"][0]["id"],"artifacts":[{"kind":"answer","seq":seq}]});
        let mut calls=tokio::task::JoinSet::new();
        for _ in 0..8 {
            let db=db.clone();let req=req.clone();
            calls.spawn(async move {call(&db,s,"zenbot",req).await.unwrap()});
        }
        let first=calls.join_next().await.unwrap().unwrap();
        while let Some(result)=calls.join_next().await {assert_eq!(first,result.unwrap());}
        assert_eq!(count(&db,"work_artifacts").await,1);
        assert_eq!(tape::load(&db,s,&["work_submitted"]).await.unwrap().len(),1);
    }

    #[tokio::test]
    async fn recovery_failure_is_atomic_and_interrupted_cannot_submit() {
        let (_d,db)=crate::test_util::database().await;
        let s=session(&db,"zenbot").await;
        let a=call(&db,s,"zenbot",create("one")).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_recovery BEFORE UPDATE ON work_attempts BEGIN SELECT RAISE(ABORT,'injected'); END").execute(&db).await.unwrap();
        assert!(recover(&db).await.is_err());
        assert!(tape::load(&db,s,&["work_interrupted"]).await.unwrap().is_empty());
        let status: String=sqlx::query_scalar("SELECT status FROM work_attempts").fetch_one(&db).await.unwrap();
        assert_eq!(status,"running");
        sqlx::query("DROP TRIGGER reject_recovery").execute(&db).await.unwrap();
        assert_eq!(recover(&db).await.unwrap(),1);
        assert!(call(&db,s,"zenbot",json!({"action":"submit","key":"x","attempt_id":a["attempts"][0]["id"],"artifacts":[{"kind":"answer","seq":100}]})).await.is_err());
    }

    #[test]
    fn contract_and_file_limits_are_enforced() {
        let mut req=create("size");
        req["contract"]["goal"]=json!("x".repeat(8193));
        let c: Contract=serde_json::from_value(req["contract"].clone()).unwrap();
        assert!(validate(&c).is_err());
        req=create("size");
        req["contract"]["criteria"]=json!(vec![req["contract"]["criteria"][0].clone();33]);
        let c: Contract=serde_json::from_value(req["contract"].clone()).unwrap();
        assert!(validate(&c).is_err());
        let root=crate::test_util::TestDir::new("work-limit");
        std::fs::File::create(root.join("large")).unwrap().set_len(16*1024*1024+1).unwrap();
        assert!(file_identity(&root,"large",None).is_err());
    }

    #[test]
    fn tool_visibility_respects_verifier_and_agent_allowlist() {
        let has=|kind|crate::agent::specs(kind).as_array().unwrap().iter().any(|v|v["name"]=="work");
        assert!(has(None));
        assert!(has(Some(crate::delegate::SUBAGENT)));
        assert!(!has(Some(crate::agent::VERIFIER)));
        assert!(agents::TOOLS.contains(&"work"));
    }
}

