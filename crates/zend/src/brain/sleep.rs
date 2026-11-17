//! The sleep (nightly, at once when short-term memory passes its size, or by hand) brings each
//! agent's short-term memory under its size (`ZEN_MEMORY_CHARS`) and keeps what matters. System One
//! judges every entry against the agent's whole short-term memory, so it can see what a later entry
//! settled, superseded or repeats, and whether the session that wrote it has ended:
//! `memory_durable` (promote: lasting, moves now; lasting: goes to long-term memory whenever it
//! leaves; keep) and `memory_disposition` (archive: finished; keep). An entry that leaves goes to
//! long-term memory (a record file of its own, citing the memory) when it is lasting, else to the
//! archive. If memory is still over its size, the entries most likely finished leave next, oldest
//! first among near-equals, until it is at four fifths of the size (room for the next entries
//! without another sleep at once). Without System One's answers nothing moves, and the note says
//! memory is still over. Nothing is deleted, no prompt file is touched, and a memory changed while
//! the sleep ran is left as it is now. Each agent's short-term memory is consolidated into its own
//! vault.
use super::*;

fn basis(source: &str) -> &'static str {
    match source {
        "owner" => "owner_statement",
        "verified" => "observed",
        _ => "inference",
    }
}

/// A record id's words from a memory's text: lowercase ASCII letters, digits and hyphens (a record
/// id allows nothing else), Portuguese and other Latin accents folded (`José` → `jose`).
fn ascii_slug(text: &str) -> String {
    let folded: String = text
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'Á' | 'À' | 'Â' | 'Ã' | 'Ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
            'ç' | 'Ç' => 'c',
            'ñ' | 'Ñ' => 'n',
            c if c.is_ascii_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    crate::wiki::slug(&folded)
}

/// Write a long-term memory record from a memory's text, in `agent`'s vault. `session` writes it (`remember` past the
/// high bar), or the kernel does (the sleep, citing the memory in `evidence`). Returns its id.
pub async fn long_term(
    db: &SqlitePool,
    agent: &str,
    text: &str,
    kind: &str,
    source: &str,
    cite: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let vault = crate::agents::vault(agent);
    let words: String = ascii_slug(&zen_proto::head(text, 60)).chars().take(48).collect();
    let base = format!("{}-{}", at.format("%Y-%m-%d"), words.trim_end_matches('-'));
    let mut id = base.clone();
    for n in 2.. {
        let taken: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brain_records WHERE id=$1)")
            .bind(&id)
            .fetch_one(db)
            .await?;
        if !taken && !records::path(&vault, "memory", kind, &id).exists() {
            break;
        }
        id = format!("{base}-{n}");
    }
    let title = zen_proto::head(&text.replace('\n', " "), 80);
    let record = json!({"id": id, "store": "memory", "kind": kind, "title": title, "body": text,
        "metadata": {"evidence": [{"source": cite, "locator": format!("short-term memory, {}", at.format("%Y-%m-%d")), "basis": basis(source)}]}});
    put(db, &vault, &records::Scope::all(), &record, None, true).await?;
    Ok(id)
}

pub async fn consolidate(app: &crate::AppState, trigger: &str) -> Result<Value> {
    let db = &app.db;
    static SLEEP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _lock = SLEEP_LOCK.try_lock().map_err(|_| anyhow::anyhow!("a sleep is already running"))?;
    let run: i64 = sqlx::query_scalar("INSERT INTO sleep_runs(trigger,scorer) VALUES($1,$2) RETURNING id")
        .bind(trigger)
        .bind(crate::s1::default_model())
        .fetch_one(db)
        .await?;
    let result = sleep(db, run).await;
    if let Err(e) = &result {
        sqlx::query("UPDATE sleep_runs SET ended_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),error=$2 WHERE id=$1")
            .bind(run)
            .bind(format!("{e:#}"))
            .execute(db)
            .await?;
    }
    result
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    text: String,
    source: String,
    record_type: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: String,
    agent: String,
    /// The session that wrote it has ended (archived): a remark from it is no longer mid-discussion.
    session_ended: bool,
}

/// Where an entry leaving short-term memory went.
enum Gone {
    Promoted,
    Archived,
    /// Promoting failed; it stays short-term.
    Stayed,
    /// Changed while the sleep ran: left as it is now.
    Changed,
}

/// Move an entry out of short-term memory: to long-term memory when it is `lasting`, else to the
/// archive. `why` says why it left: "finished", "to fit the size", or "" when it is promoted for itself. An entry changed since it was read is left alone.
async fn leave(db: &SqlitePool, r: &Row, lasting: bool, why: &str) -> Result<Gone> {
    let archive = |reason: String| {
        sqlx::query("UPDATE memories SET tier='archived',reason=$3,updated_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id=$1 AND updated_at=$2 AND tier='short'")
            .bind(r.id)
            .bind(r.updated_at.clone())
            .bind(reason)
            .execute(db)
    };
    let fresh: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memories WHERE id=$1 AND updated_at=$2 AND tier='short')")
        .bind(r.id)
        .bind(&r.updated_at)
        .fetch_one(db)
        .await?;
    if !fresh {
        return Ok(Gone::Changed);
    }
    if !lasting {
        let n = archive(format!("the sleep: {why}")).await?.rows_affected();
        return Ok(if n == 1 { Gone::Archived } else { Gone::Changed });
    }
    match long_term(db, &r.agent, &r.text, &r.record_type, &r.source, &format!("m{}", r.id), r.created_at).await {
        Ok(id) => {
            let why = if why.is_empty() { String::new() } else { format!(" ({why})") };
            archive(format!("promoted to long-term memory {id}{why}")).await?;
            Ok(Gone::Promoted)
        }
        Err(e) => {
            tracing::warn!("promoting m{}: {e:#}", r.id);
            Ok(Gone::Stayed)
        }
    }
}

/// Which entries leave short-term memory to bring it to `target` characters: the least active
/// first (the chance it is finished, in tenths, so near-equals go by age), the oldest among
/// equals. `entries` are (characters, chance it is finished, created). Returns their indices.
fn to_fit(entries: &[(usize, f64, chrono::DateTime<chrono::Utc>)], used: usize, target: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    let tenths = |p: f64| (p * 10.0).round() as i64;
    order.sort_by(|&a, &b| tenths(entries[b].1).cmp(&tenths(entries[a].1)).then(entries[a].2.cmp(&entries[b].2)));
    let mut used = used;
    let mut out = Vec::new();
    for i in order {
        if used <= target {
            break;
        }
        used = used.saturating_sub(entries[i].0);
        out.push(i);
    }
    out
}

/// What the sleep shows System One of an agent's short-term memory, so it can tell what a later
/// entry settled, superseded or repeats.
fn listing(rows: &[&Row], now: chrono::DateTime<chrono::Utc>) -> Value {
    rows.iter()
        .map(|r| {
            let ended = if r.session_ended { ", session ended" } else { "" };
            json!(format!("m{} ({} days old, {}{ended}): {}", r.id, (now - r.created_at).num_days(), r.source, r.text.replace('\n', " ")))
        })
        .collect()
}

/// Every agent with short-term memory or a vault: zenbot first.
async fn agents_to_tend(db: &SqlitePool) -> Result<Vec<String>> {
    let mut names: Vec<String> = sqlx::query_scalar("SELECT DISTINCT agent FROM memories WHERE tier='short'").fetch_all(db).await?;
    names.extend(crate::agents::vaults().into_iter().map(|(n, _)| n));
    names.retain(|n| n != crate::agents::DEFAULT);
    names.sort();
    names.dedup();
    names.insert(0, crate::agents::DEFAULT.into());
    Ok(names)
}

async fn sleep(db: &SqlitePool, run: i64) -> Result<Value> {
    let agents = agents_to_tend(db).await?;
    for a in &agents {
        crate::memory::import(db, a).await?;
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT m.id, m.text, m.source, m.record_type, m.created_at, m.updated_at, m.agent, COALESCE(s.archived, 0) AS session_ended
         FROM memories m LEFT JOIN sessions s ON s.id = m.session_id WHERE m.tier='short' ORDER BY m.id",
    )
    .fetch_all(db)
    .await?;
    let (mut promoted, mut archived, mut kept, mut changed, mut fitted) = (0, 0, 0, 0, 0);
    let now = chrono::Utc::now();
    let mut over = Vec::new();
    for agent in &agents {
        let mine: Vec<&Row> = rows.iter().filter(|r| &r.agent == agent).collect();
        if mine.is_empty() {
            continue;
        }
        let facts = crate::s1::Facts::new(None).with("memories", listing(&mine, now));
        // The entries the System One pass leaves short-term, with its answers.
        let mut left: Vec<(&Row, bool, Option<f64>)> = Vec::new();
        for batch in mine.chunks(12) {
            let items: Vec<Value> = batch
                .iter()
                .map(|r| json!({"id": format!("m{}", r.id), "memory": r.text, "kind": r.record_type, "source": r.source,
                    "age_days": (now - r.created_at).num_days(), "session_ended": r.session_ended}))
                .collect();
            let (durable, disposition) = tokio::join!(
                crate::s1::decide_items(db, "memory_durable", facts.clone(), items.clone()),
                crate::s1::decide_items(db, "memory_disposition", facts.clone(), items)
            );
            for (i, r) in batch.iter().enumerate() {
                let lasting = durable.get(i).is_some_and(|d| d.is("promote") || d.is("lasting"));
                let why = if durable.get(i).is_some_and(|d| d.is("promote")) {
                    Some("")
                } else if disposition.get(i).is_some_and(|d| d.is("archive")) {
                    Some("finished")
                } else {
                    None
                };
                match why {
                    Some(why) => match leave(db, r, lasting, why).await? {
                        Gone::Promoted => promoted += 1,
                        Gone::Archived => archived += 1,
                        Gone::Stayed => kept += 1,
                        Gone::Changed => changed += 1,
                    },
                    None => {
                        let done = disposition.get(i).filter(|d| d.answered);
                        left.push((r, lasting, done.map(|d| d.answers["disposition"]["probabilities"]["archive"].as_f64().unwrap_or(0.0))));
                        kept += 1;
                    }
                }
            }
        }
        // Still over the size: the least active entries System One answered about leave, lasting
        // ones to long-term memory and the rest to the archive, until it fits with room to spare
        // (so the next entries don't start another sleep at once). Without answers nothing moves.
        let target = crate::memory::cap() * 4 / 5;
        let used = crate::memory::used_chars(db, agent).await;
        if used > crate::memory::cap() {
            let judged: Vec<&(&Row, bool, Option<f64>)> = left.iter().filter(|(_, _, done)| done.is_some()).collect();
            let sizes: Vec<_> = judged.iter().map(|(r, _, done)| (r.text.chars().count() + 10, done.unwrap_or(0.0), r.created_at)).collect();
            for i in to_fit(&sizes, used, target) {
                let (r, lasting, _) = judged[i];
                match leave(db, r, *lasting, "to fit the size").await? {
                    Gone::Promoted => promoted += 1,
                    Gone::Archived => archived += 1,
                    Gone::Stayed => continue,
                    Gone::Changed => {
                        (changed, kept) = (changed + 1, kept - 1);
                        continue;
                    }
                }
                kept -= 1;
                fitted += 1;
            }
            let used = crate::memory::used_chars(db, agent).await;
            if used > crate::memory::cap() {
                over.push(format!("{agent}'s is still {used} of {} characters (System One didn't answer for the rest)", crate::memory::cap()));
            }
        }
    }
    let mut note = format!(
        "{promoted} promoted to long-term memory, {archived} archived, {kept} kept, {changed} changed during the sleep and left as they are."
    );
    if fitted > 0 {
        note.push_str(&format!(" {fitted} of those moved to fit the size."));
    }
    if !over.is_empty() {
        note.push_str(&format!(" Over the size: {}.", over.join("; ")));
    }
    sqlx::query("UPDATE sleep_runs SET ended_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),entries=$2,kept=$3,promoted=$4,dropped=$5,note=$6 WHERE id=$1")
        .bind(run)
        .bind(rows.len() as i32)
        .bind(kept)
        .bind(promoted)
        .bind(archived)
        .bind(&note)
        .execute(db)
        .await?;
    for a in agents.iter().filter(|a| *a == crate::agents::DEFAULT || crate::agents::vault(a).is_dir()) {
        crate::memory::export(db, a).await?;
    }
    Ok(
        json!({"run": run, "entries": rows.len(), "promoted": promoted, "archived": archived, "kept": kept, "changed": changed, "fitted": fitted, "note": note}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_ids_are_ascii() {
        assert_eq!(ascii_slug("So I am José, the São Paulo ação — 日本"), "so-i-am-jose-the-sao-paulo-acao");
    }

    #[test]
    fn the_least_active_leave_first_and_only_until_it_fits() {
        let day = |d: i64| chrono::DateTime::<chrono::Utc>::from_timestamp(d * 86_400, 0).unwrap();
        // (characters, chance it is finished, created)
        let entries = [(100, 0.1, day(1)), (100, 0.8, day(5)), (100, 0.82, day(2)), (100, 0.4, day(3)), (100, 0.1, day(4))];
        // Finished-looking first, the older of two near-equals first; it stops once it fits.
        assert_eq!(to_fit(&entries, 500, 300), vec![2, 1]);
        assert_eq!(to_fit(&entries, 500, 250), vec![2, 1, 3]);
        // Then by age among equals, and never more than there is.
        assert_eq!(to_fit(&entries, 500, 0), vec![2, 1, 3, 0, 4]);
        assert!(to_fit(&entries, 300, 300).is_empty());
    }
}
