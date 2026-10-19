//! Permission snapshots shared by a tool call's nested checks. A fresh snapshot starts every
//! call; relevant mutations invalidate all in-flight snapshots. Final handover still queries
//! contamination live. This avoids retaining authority across a turn's arbitrary outside reads.
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Snapshot { pub session: Uuid, pub risk: i16, pub lock: Option<String>, pub stopped: bool, epoch: u64 }
static EPOCH: AtomicU64 = AtomicU64::new(0);
tokio::task_local! { static STATE: RefCell<Option<Snapshot>>; }

pub async fn scope<F: std::future::Future>(future: F) -> F::Output {
    STATE.scope(RefCell::new(None), future).await
}
pub fn invalidate() { EPOCH.fetch_add(1, Ordering::SeqCst); }

pub async fn get(db: &PgPool, session: Uuid) -> anyhow::Result<Snapshot> {
    let epoch = EPOCH.load(Ordering::SeqCst);
    if let Ok(Some(state)) = STATE.try_with(|state| state.borrow().clone()) {
        if state.session == session && state.epoch == epoch { return Ok(state); }
    }
    let row = sqlx::query("SELECT risk,lock_reason, risk>=5 OR zen_session_contaminated(id) AS stopped FROM sessions WHERE id=$1")
        .bind(session).fetch_one(db).await?;
    let state = Snapshot { session, risk: row.get("risk"), lock: row.get("lock_reason"), stopped: row.get("stopped"), epoch };
    let _ = STATE.try_with(|cached| *cached.borrow_mut() = Some(state.clone()));
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "SOURCE_TEST_DB_URL must name a disposable database"]
    async fn permission_snapshot_database() {
        let db = PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model) VALUES($1,'faux/smoke')").bind(id).execute(&db).await.unwrap();
        scope(async {
            assert_eq!(get(&db,id).await.unwrap().risk,1);
            crate::sources::raise(&db,id,4).await.unwrap();
            assert_eq!(get(&db,id).await.unwrap().risk,4);
            sqlx::query("UPDATE sessions SET lock_reason='held' WHERE id=$1").bind(id).execute(&db).await.unwrap();
            invalidate();
            assert_eq!(get(&db,id).await.unwrap().lock.as_deref(),Some("held"));
            let app = crate::test_util::app(db.clone());
            crate::taint::deny(&app,id,"test").await.unwrap();
            assert!(get(&db,id).await.unwrap().stopped);
        }).await;
        db.close().await;
    }
}
